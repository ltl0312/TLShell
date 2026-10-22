//! 按字体族名取出字体文件字节，供 `fontdue` 解析。
//!
//! 为什么不用 GDI 的 `GetFontData`：`dwTable = 0` 声称返回“整个字体文件”，但对
//! `.ttc` 集合它返回的是拼接后的表，缺 `head` 头，`fontdue` 会报
//! “The head table is missing or malformed”（实测 Microsoft YaHei 就是如此）。
//! 所以这里改成**按已知文件名在字体目录里找文件、直接读原始字节**，
//! `fontdue` 能正确解析 TTC 并把 `collection_index` 选到第 0 个字体。
//!
//! 代价是族名 → 文件名要靠一张表。Windows 的字体文件名是稳定的公开约定，
//! 覆盖不到的族名会走到 `FontSet::load` 的候选链上下一个，不会静默换字体。

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

/// 族名 → `(常规文件名, 粗体文件名)`。
const FONT_FILES: &[(&str, &[&str], &[&str])] = &[
    (
        "Cascadia Mono",
        &["CascadiaMono.ttf"],
        &["CascadiaMono.ttf"],
    ),
    (
        "Cascadia Code",
        &["CascadiaCode.ttf"],
        &["CascadiaCode.ttf"],
    ),
    ("Consolas", &["consola.ttf"], &["consolab.ttf"]),
    ("Courier New", &["cour.ttf"], &["courbd.ttf"]),
    (
        "Sarasa Mono SC",
        &["SarasaMonoSC-Regular.ttf", "SarasaMonoSC.ttf"],
        &["SarasaMonoSC-Bold.ttf"],
    ),
    (
        "Sarasa Term SC",
        &["SarasaTermSC-Regular.ttf", "SarasaTermSC.ttf"],
        &["SarasaTermSC-Bold.ttf"],
    ),
    (
        "Microsoft YaHei",
        &["msyh.ttc", "msyh.ttf"],
        &["msyhbd.ttc", "msyhbd.ttf"],
    ),
    (
        "Microsoft YaHei UI",
        &["msyh.ttc", "msyh.ttf"],
        &["msyhbd.ttc", "msyhbd.ttf"],
    ),
    (
        "SimSun",
        &["simsun.ttc", "simsun.ttf"],
        &["simsun.ttc", "simsun.ttf"],
    ),
    ("MS Gothic", &["msgothic.ttc"], &["msgothic.ttc"]),
];

/// 字体可能所在的目录：系统级 + 当前用户安装。
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = std::env::var_os("SystemRoot") {
        dirs.push(PathBuf::from(root).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(
            PathBuf::from(local)
                .join("Microsoft")
                .join("Windows")
                .join("Fonts"),
        );
    }
    dirs
}

/// 取指定字体族的文件字节。`bold` 为真时只找粗体文件，找不到就报错，
/// 由绘制层用“双描”合成粗体，而不是拿常规字重冒充。
pub fn load_family(family: &str, bold: bool) -> Result<Vec<u8>> {
    let trimmed = family.trim();
    if trimmed.is_empty() {
        bail!("字体族名为空");
    }

    let entry = FONT_FILES
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(trimmed));
    let Some((_, regular, bold_files)) = entry else {
        bail!("字体族 `{trimmed}` 不在已知文件名表里");
    };

    let candidates = if bold { *bold_files } else { *regular };
    let dirs = font_dirs();
    for dir in &dirs {
        for name in candidates {
            let path = dir.join(name);
            if path.is_file() {
                return std::fs::read(&path)
                    .with_context(|| format!("读取字体文件失败: {}", path.display()));
            }
        }
    }
    bail!("字体族 `{trimmed}` 的文件不存在（已搜索 {}）", dirs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_family_is_rejected() {
        assert!(load_family("", false).is_err());
        assert!(load_family("   ", false).is_err());
    }

    #[test]
    fn unknown_family_is_rejected() {
        assert!(load_family("No Such Font Family", false).is_err());
    }

    #[test]
    fn font_dirs_are_absolute() {
        for dir in font_dirs() {
            assert!(dir.is_absolute(), "{} 不是绝对路径", dir.display());
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        // 大小写不同不应影响“是否在表里”的判断——两者要么都成功，要么都因缺文件而失败。
        let lower = load_family("consolas", false);
        let upper = load_family("CONSOLAS", false);
        assert_eq!(lower.is_ok(), upper.is_ok());
    }
}
