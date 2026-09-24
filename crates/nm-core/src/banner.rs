//! 启动 banner：渐变字符画打印（思路抽取自 cmx-web-chassis，泛化为库内可复用组件）。
//!
//! 各守护进程可提供**自己的字符画 + 标语 + 渐变配色 + 右下角签名**（[`BannerSpec`]）；目前仅
//! `nmd` 接入（NANO MESH），其余 bin 日后一行即可接入。**非终端**（stdout 重定向到文件/日志管道，
//! 如 `nohup`）自动降级为纯文本，避免 ANSI 转义码污染日志。

use std::io::IsTerminal;

/// 一个 RGB 颜色停靠点。
pub type Rgb = (u8, u8, u8);

/// 默认渐变停靠点：青 → 蓝 → 紫（未指定 `stops` 时使用）。
pub const DEFAULT_STOPS: [Rgb; 3] = [(0, 229, 255), (41, 121, 255), (124, 77, 255)];

/// banner 描述：字符画 + 标语 + 渐变停靠点 + 右下角签名。各守护进程自定义。
#[derive(Debug, Clone)]
pub struct BannerSpec {
    /// 字符画（多行）。
    pub art: String,
    /// 字符画下方标语。空则不打印。
    pub tagline: String,
    /// 右下角签名小字（暗淡色 + 右对齐到字符画宽度）。空则不打印。
    pub signature: String,
    /// 纵向渐变停靠点（≥1 个；1 个即纯色，多个按内容行位置线性插值）。
    pub stops: Vec<Rgb>,
}

impl BannerSpec {
    /// 以字符画构建（标语/签名默认空，渐变用 [`DEFAULT_STOPS`]）。
    pub fn new(art: impl Into<String>) -> Self {
        Self {
            art: art.into(),
            tagline: String::new(),
            signature: String::new(),
            stops: DEFAULT_STOPS.to_vec(),
        }
    }

    /// 覆盖标语。
    pub fn tagline(mut self, tagline: impl Into<String>) -> Self {
        self.tagline = tagline.into();
        self
    }

    /// 覆盖右下角签名小字（右对齐到字符画宽度，暗淡色）。
    pub fn signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = signature.into();
        self
    }

    /// 覆盖渐变配色（≥1 个停靠点）。空则忽略（保留原配色）。
    pub fn stops(mut self, stops: Vec<Rgb>) -> Self {
        if !stops.is_empty() {
            self.stops = stops;
        }
        self
    }
}

/// 打印一个 banner（带纵向渐变；非终端降级纯文本）。
pub fn print(spec: &BannerSpec) {
    // 非终端：纯文本，避免 ANSI 码污染日志（nohup / 管道 / 重定向到文件）。
    if !std::io::stdout().is_terminal() {
        println!("{}", spec.art);
        if !spec.tagline.is_empty() {
            println!("{}", spec.tagline);
        }
        if !spec.signature.is_empty() {
            println!("{}", spec.signature);
        }
        return;
    }

    let stops = if spec.stops.is_empty() {
        &DEFAULT_STOPS[..]
    } else {
        &spec.stops[..]
    };

    let lines: Vec<&str> = spec.art.lines().collect();
    let total = lines.iter().filter(|l| !l.trim().is_empty()).count();
    let denom = total.saturating_sub(1).max(1) as f32;

    // 字符画显示宽度（用于右下角签名右对齐）：取最宽内容行的显示列数。
    let art_width = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);

    let mut content_idx = 0usize;
    for line in &lines {
        if line.trim().is_empty() {
            println!();
            continue;
        }
        let t = content_idx as f32 / denom;
        let (r, g, b) = gradient_color(stops, t);
        println!("\x1b[1;38;2;{r};{g};{b}m{line}\x1b[0m");
        content_idx += 1;
    }

    if !spec.tagline.is_empty() {
        let (r, g, b) = *stops.last().unwrap_or(&(255, 255, 255));
        println!("\n\x1b[1;38;2;{r};{g};{b}m{}\x1b[0m", spec.tagline);
    }

    // 右下角签名小字：右对齐到字符画宽度 + 暗淡（\x1b[2m）显得更小/次要。
    if !spec.signature.is_empty() {
        let pad = art_width.saturating_sub(display_width(&spec.signature));
        println!("\x1b[2m{}{}\x1b[0m", " ".repeat(pad), spec.signature);
    }
}

/// 估算字符串的终端显示宽度（东亚全角计 2 列，其余 1 列）。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            if (0x1100..=0x115F).contains(&cp)
                || (0x2E80..=0xA4CF).contains(&cp)
                || (0xAC00..=0xD7A3).contains(&cp)
                || (0xF900..=0xFAFF).contains(&cp)
                || (0xFF00..=0xFF60).contains(&cp)
                || (0xFFE0..=0xFFE6).contains(&cp)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 在 RGB 停靠点间按 `t ∈ [0,1]` 线性插值。
fn gradient_color(stops: &[Rgb], t: f32) -> Rgb {
    let seg = stops.len().saturating_sub(1);
    if seg == 0 {
        return stops.first().copied().unwrap_or((255, 255, 255));
    }
    let scaled = t.clamp(0.0, 1.0) * seg as f32;
    let i = (scaled.floor() as usize).min(seg - 1);
    let local = scaled - i as f32;
    let (r0, g0, b0) = stops[i];
    let (r1, g1, b1) = stops[i + 1];
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * local).round() as u8;
    (lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}
