//! ConPTY 会话：启动子进程、双向搬运字节、退出时收尸。
//!
//! 线程模型（计划书 §5.3-4）：**只有 UI 线程持有 `Term`**。读线程不做任何解析，
//! 它只把原始字节块通过 channel 交回来，由 UI 线程喂给 VT 状态机。
//!
//! 生命周期（安全评审 P0-6）：子进程被放进一个 `KILL_ON_JOB_CLOSE` 的 Job Object，
//! 因此 TLShell 被强杀时 shell 不会变成孤儿。

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::term::shell::ResolvedShell;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// 单次读取的缓冲大小。ConPTY 的输出通常是几百字节一块，8 KiB 足够摊平 syscall 开销。
const READ_BUFFER: usize = 8192;

/// 一个 `KILL_ON_JOB_CLOSE` 的作业对象句柄。
///
/// 句柄关闭（含进程被强杀）时，内核会杀掉作业里的所有进程。
struct JobHandle(HANDLE);

// SAFETY: HANDLE 是内核对象句柄，所有权唯一且可跨线程转移；JobHandle 不实现 Clone，
// 也不会被多线程同时访问，因此跨线程移动是安全的。
unsafe impl Send for JobHandle {}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 CreateJobObjectW 返回且只在这里关闭一次。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn create_kill_on_close_job() -> Result<JobHandle> {
    // SAFETY: 无入参指针；返回的句柄所有权交给 JobHandle。
    let job = unsafe { CreateJobObjectW(None, None) }.context("CreateJobObjectW 失败")?;
    let job = JobHandle(job);

    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: info 是本地栈变量，长度与结构体一致，调用期间不会被移动。
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .context("SetInformationJobObject 失败")?;
    }
    Ok(job)
}

fn assign_to_job(job: &JobHandle, pid: u32) -> Result<()> {
    // SAFETY: 只按 pid 打开进程，句柄用后立即关闭。
    unsafe {
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)
            .with_context(|| format!("OpenProcess(pid={pid}) 失败"))?;
        let result = AssignProcessToJobObject(job.0, process)
            .with_context(|| format!("AssignProcessToJobObject(pid={pid}) 失败"));
        let _ = CloseHandle(process);
        result
    }
}

/// 启动会话所需的一切。
pub struct SpawnOptions {
    pub shell: ResolvedShell,
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
    /// 已校验过的启动目录（见 `shell::resolve_cwd`），不再允许为空。
    pub cwd: PathBuf,
    /// 已过滤的子进程环境（见 `shell::child_env`），父环境不整体继承。
    pub env: Vec<(OsString, OsString)>,
}

/// 一个活着的 PTY 会话。
pub struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    job: JobHandle,
    pid: u32,
    exit_code: Option<u32>,
}

impl Session {
    pub fn spawn(options: SpawnOptions) -> Result<Self> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: options.rows.max(1),
            cols: options.cols.max(1),
            pixel_width: options.pixel_width,
            pixel_height: options.pixel_height,
        };
        let pair = pty_system.openpty(size).context("openpty 失败")?;

        let mut command = CommandBuilder::new(&options.shell.program);
        command.args(&options.shell.args);
        command.cwd(&options.cwd);
        // 先清空再逐项写入：继承父环境会把凭据、令牌、各类 *_proxy
        // 和 NODE_OPTIONS 之类的注入点原样交给子进程（S-03）。
        command.env_clear();
        for (key, value) in &options.env {
            command.env(key, value);
        }
        // 显式设置 TERM，避免某些程序按 dumb 终端降级。
        command.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("启动 {} 失败", options.shell.program.display()))?;
        // slave 必须尽早丢弃，否则读端永远等不到 EOF。
        drop(pair.slave);

        let pid = child.process_id().unwrap_or(0);
        let job = create_kill_on_close_job()?;
        if pid != 0 {
            // 作业分配失败不该让整个终端起不来：记下来，退出时仍然显式 kill。
            if let Err(err) = assign_to_job(&job, pid) {
                crate::diag::warn(&format!("子进程未加入作业对象: {err:#}"));
            }
        }

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("try_clone_reader 失败")?;
        let writer = pair.master.take_writer().context("take_writer 失败")?;

        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("tlshell-pty-reader".to_owned())
            .spawn(move || {
                let mut buffer = vec![0u8; READ_BUFFER];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(buffer[..n].to_vec()).is_err() {
                                // UI 线程已经走了，读下去没有意义。
                                break;
                            }
                        }
                        Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .context("创建 PTY 读线程失败")?;

        Ok(Self {
            master: pair.master,
            writer,
            child,
            rx,
            reader: Some(handle),
            job,
            pid,
            exit_code: None,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 把字节写进 PTY。写入失败通常意味着 shell 已退出，不是致命错误。
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).context("写入 PTY 失败")?;
        self.writer.flush().context("flush PTY 失败")
    }

    /// 取走当前已到达的所有输出块。
    pub fn drain(&self) -> Vec<Vec<u8>> {
        let mut chunks = Vec::new();
        while let Ok(chunk) = self.rx.try_recv() {
            chunks.push(chunk);
        }
        chunks
    }

    pub fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width,
                pixel_height,
            })
            .context("PTY resize 失败")
    }

    /// 非阻塞地检查子进程是否退出；返回退出码（只返回一次）。
    pub fn try_exit_code(&mut self) -> Option<u32> {
        if self.exit_code.is_some() {
            return self.exit_code;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit_code = Some(exit_code_of(status));
                self.exit_code
            }
            Ok(None) => None,
            Err(_) => {
                self.exit_code = Some(1);
                self.exit_code
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 先显式 kill，再关作业对象；两道保险。
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
        // job 的 Drop 会关句柄，进而杀掉残留进程。
        let _ = &self.job;
    }
}

fn exit_code_of(status: portable_pty::ExitStatus) -> u32 {
    status.exit_code()
}
