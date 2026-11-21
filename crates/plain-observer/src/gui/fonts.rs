use std::path::{Path, PathBuf};
use std::{env, fs};

use ab_glyph::{Font as _, FontRef};
use eframe::egui::{Context, FontData, FontDefinitions};
use ttf_parser::{name_id, Face};

// Require visible outlines for the observer's Chinese labels (including telemetry).
// The default egui fonts cover the Latin text and the toolbar symbols.
const REQUIRED_CHINESE_GLYPHS: &str = concat!(
    "遗电平原 开发者实时观测器 暂停运行 细步宏步 游戏日 完整四纪元 ",
    "细步帧 纪元切换断点 流域边界 遗迹锚点天坑 图层 世界尺寸 周期配置 ",
    "全局地图总览 点击跳转 守恒与宏观流域遥测 种群位预留 全图总水量 漂移 ",
    "地表地下 大气水汽 盐壳格 活跃休眠脏存盘 气象历时湿度肥 ",
    "网格物理与微环境探针 左键主视图 坐标材质 屈服酸蚀完整度 流体温 ",
    "菌群腐导盐铁 墙阻掩码 电势梯度 任意位置检查权威状态。",
    "实时分系统性能分析器 最新步耗时 明细 电场宏气候追平 活跃块估计内核内存 ",
    "第一纪元：盐壳静电荒漠 第二纪元静默逆雨浮尘 第三纪元铁锈红雨洪泛 ",
    "第四纪元花海鼎盛球雷 平稳静电风 浮尘潮 游走球雷贴地电雾 极光磁暴幻影夜 铁锈红雨酸蚀暴 ",
    "综合地形与水体 材料屈服强度物理属性 酸碱度场 土壤肥力电解质 含盐度场 温度与地热场 ",
    "微生物腐生分解菌 导电丝菌毯 固盐产气藻菌 蚀铁矿化菌 局部物理电势场 脏区 自然群系涌现宜居度 ",
    "断脊高原 钙华峡谷 电解盐盆 断层荒谷 花海繁荣 锈红酸雨 播放 守恒总账",
);

pub(super) fn install(ctx: &Context) -> Option<(PathBuf, u32)> {
    let (path, font) = env::var_os("PLAIN_OBSERVER_FONT")
        .and_then(|path| candidate(path.into(), None))
        .or_else(fontconfig_font)
        .or_else(system_font)?;

    let index = font.index;
    let mut fonts = FontDefinitions::default();
    let name = "Aftervolt CJK".to_owned();
    fonts.font_data.insert(name.clone(), font.into());
    for family in fonts.families.values_mut() {
        family.push(name.clone());
    }
    ctx.set_fonts(fonts);
    Some((path, index))
}

fn candidate(path: PathBuf, index: Option<u32>) -> Option<(PathBuf, FontData)> {
    if !path.is_file() {
        return None;
    }
    let bytes = fs::read(&path).ok()?;
    let index = usable_face(&bytes, index)?;
    let mut font = FontData::from_owned(bytes);
    font.index = index;
    Some((path, font))
}

fn usable_face(bytes: &[u8], index: Option<u32>) -> Option<u32> {
    let count = if let Some(count) = ttf_parser::fonts_in_collection(bytes) {
        // A TTC header needs one four-byte offset for each declared face.
        if count as usize > bytes.len().saturating_sub(12) / 4 {
            return None;
        }
        count
    } else {
        1
    };
    let simplified_faces: Vec<_> = (0..count)
        .filter(|&index| count > 1 && is_simplified_face(bytes, index))
        .collect();

    // Fontconfig supplies an exact index. For a bare TTC path, use face names to
    // distinguish regional variants that share the same Unicode code points.
    let indices = if let Some(index) = index {
        if !simplified_faces.is_empty() && !simplified_faces.contains(&index) {
            return None;
        }
        vec![index]
    } else if simplified_faces.is_empty() {
        (0..count).collect()
    } else {
        simplified_faces
    };

    indices.into_iter().find(|&index| {
        // epaint uses ab_glyph and panics on parse errors. A cmap entry alone
        // also isn't enough: epaint 0.31 rasterizes outlines, not bitmap strikes.
        FontRef::try_from_slice_and_index(bytes, index).is_ok_and(|font| {
            font.units_per_em().is_some()
                && font.height_unscaled() > 0.0
                && REQUIRED_CHINESE_GLYPHS
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .all(|c| {
                        let glyph = font.glyph_id(c);
                        glyph.0 != 0
                            && font.h_advance_unscaled(glyph) > 0.0
                            && font.outline(glyph).is_some()
                    })
        })
    })
}

fn is_simplified_face(bytes: &[u8], index: u32) -> bool {
    Face::parse(bytes, index).is_ok_and(|face| {
        face.names()
            .into_iter()
            .filter(|name| {
                matches!(
                    name.name_id,
                    name_id::FAMILY | name_id::TYPOGRAPHIC_FAMILY | name_id::POST_SCRIPT_NAME
                )
            })
            .filter_map(|name| name.to_string())
            .any(|name| {
                let name = name.to_ascii_lowercase();
                name.contains("simplified chinese")
                    || name.contains("简体")
                    || name.split_ascii_whitespace().any(|word| word == "sc")
                    || [
                        "pingfangsc",
                        "notosanscjksc",
                        "notoserifcjksc",
                        "sourcehansanssc",
                    ]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
            })
    })
}

#[cfg(target_os = "linux")]
fn fontconfig_font() -> Option<(PathBuf, FontData)> {
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::process::Command;

    const FAMILIES: &[&str] = &[
        "Noto Sans CJK SC",
        "Source Han Sans SC",
        "WenQuanYi Zen Hei",
        "WenQuanYi Micro Hei",
        "Microsoft YaHei",
        "Noto Sans SC",
        "STXihei",
        "sans-serif",
    ];

    let mut seen = HashSet::new();
    for &family in FAMILIES {
        let generic = family == "sans-serif";
        let mut command = Command::new("fc-match");
        if generic {
            // Keep alternatives that cover the same code points: they can use
            // different outline formats and only some may work with epaint.
            command.arg("-a");
        }
        let output = command
            .args(["-f", "%{file}\t%{index}\t%{family}\t%{lang}\n"])
            .arg(format!("{family}:lang=zh-cn"))
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }

        for row in output.stdout.split(|&byte| byte == b'\n') {
            let mut fields = row.splitn(4, |&byte| byte == b'\t');
            let (Some(file), Some(index), Some(names), Some(languages)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(index) = String::from_utf8_lossy(index).parse::<u32>() else {
                continue;
            };
            let file = PathBuf::from(OsStr::from_bytes(file));
            // Fontconfig encodes a variable font's named instance in the
            // high bits. FontData indexes the collection face only.
            let index = index & 0xffff;
            // Fontconfig can return an unrelated family for a named query.
            let matches = if generic {
                languages
                    .split(|&byte| byte == b'|')
                    .any(|lang| lang == b"zh-cn")
            } else {
                names
                    .split(|&byte| byte == b',')
                    .any(|name| name.eq_ignore_ascii_case(family.as_bytes()))
            };
            if matches && seen.insert((file.clone(), index)) {
                if let Some(font) = candidate(file, Some(index)) {
                    return Some(font);
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn fontconfig_font() -> Option<(PathBuf, FontData)> {
    None
}

fn system_font() -> Option<(PathBuf, FontData)> {
    let mut roots = Vec::new();
    #[cfg(target_os = "linux")]
    {
        roots.extend([
            PathBuf::from("/usr/share/fonts"),
            PathBuf::from("/usr/local/share/fonts"),
        ]);
        if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
            roots.push(PathBuf::from(data_home).join("fonts"));
        }
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            roots.extend([home.join(".local/share/fonts"), home.join(".fonts")]);
        }
    }
    #[cfg(target_os = "macos")]
    {
        roots.extend([
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/Library/Fonts"),
        ]);
        if let Some(home) = env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("Library/Fonts"));
        }
    }
    #[cfg(target_os = "windows")]
    {
        let windows =
            env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        roots.push(windows.join("Fonts"));
        if let Some(local) = env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
    }

    let mut files = Vec::new();
    for root in roots {
        collect_known_fonts(&root, &mut files);
    }
    files.sort_unstable();
    files
        .into_iter()
        .find_map(|(_, path)| candidate(path, None))
}

fn collect_known_fonts(dir: &Path, files: &mut Vec<(usize, PathBuf)>) {
    const FONT_NAMES: &[&str] = &[
        "notosanscjk-regular",
        "notosanssc",
        "sourcehansanssc",
        "sourcehansans-regular",
        "sourcehansans",
        "wqy-zenhei",
        "wqy-microhei",
        "msyh",
        "stxihei",
        "pingfang",
        "heitisc",
        "stsong",
        "songti",
        "simhei",
        "simsun",
        "droidsansfallback",
    ];

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_known_fonts(&path, files);
            continue;
        }
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ["ttf", "otf", "ttc"]
                    .iter()
                    .any(|kind| ext.eq_ignore_ascii_case(kind))
            })
        {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            let stem = stem.to_ascii_lowercase();
            if let Some(priority) = FONT_NAMES.iter().position(|name| stem.starts_with(name)) {
                files.push((priority, path));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use eframe::egui::{Color32, FontFamily, FontId, RawInput};

    use super::*;

    #[test]
    fn observer_text_rasterizes_in_both_font_families() {
        let ctx = Context::default();
        let (path, index) = install(&ctx)
            .expect("install a Simplified Chinese font or point PLAIN_OBSERVER_FONT to one");
        eprintln!("observer CJK font: {} (face {index})", path.display());

        const SAMPLE: &str = "遗电平原 开发者实时观测器 流域1·电解盐盆 花海繁荣 盐壳荒漠 静默逆雨 锈红酸雨 播放 暂停 守恒总账 (F9) — µ ▶ ⏸ 0123456789";
        let characters: BTreeSet<_> = [
            SAMPLE,
            REQUIRED_CHINESE_GLYPHS,
            include_str!("../gui.rs"),
            include_str!("../../../plain-contracts/src/telemetry.rs"),
            include_str!("../../../plain-worldgen/src/macro_layout.rs"),
        ]
        .into_iter()
        .flat_map(str::chars)
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .collect();

        // set_fonts is applied at the start of the next egui pass.
        let _ = ctx.run(RawInput::default(), |ctx| {
            ctx.fonts(|fonts| {
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    let font_id = FontId::new(16.0, family);
                    let mixed =
                        fonts.layout_no_wrap(SAMPLE.to_owned(), font_id.clone(), Color32::WHITE);
                    assert_eq!(mixed.rows.len(), 1);
                    assert_eq!(mixed.rows[0].glyphs.len(), SAMPLE.chars().count());
                    for &c in &characters {
                        assert!(fonts.has_glyph(&font_id, c), "{font_id:?} missing {c:?}");
                        let galley =
                            fonts.layout_no_wrap(c.to_string(), font_id.clone(), Color32::WHITE);
                        let glyph = &galley.rows[0].glyphs[0];
                        assert_eq!(glyph.chr, c);
                        assert!(glyph.advance_width > 0.0, "{font_id:?} {c:?}");
                        assert!(
                            glyph.uv_rect.max[0] > glyph.uv_rect.min[0]
                                && glyph.uv_rect.max[1] > glyph.uv_rect.min[1],
                            "{font_id:?} {c:?} has no atlas pixels"
                        );
                    }
                }
            });
        });
    }

    #[test]
    fn rejects_invalid_bytes_indices_and_fonts_without_chinese_outlines() {
        assert_eq!(usable_face(b"not a font", None), None);
        assert_eq!(usable_face(b"ttcf\0\x01\0\0\xff\xff\xff\xff", None), None);
        let defaults = FontDefinitions::default();
        let latin = &defaults.font_data["Hack"].font;
        assert!(FontRef::try_from_slice_and_index(latin, 0).is_ok());
        assert_eq!(usable_face(latin, Some(0)), None);
        assert_eq!(usable_face(latin, Some(1)), None);
    }
}
