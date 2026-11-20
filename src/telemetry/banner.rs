use crate::config::{LogFormat, LoggingConfig};
use crate::telemetry::ansi::{BOLD, DIM, RESET};

const LOGO: &str = include_str!("kairo-ascii.txt");

const TAGLINE: &str = "Standalone Discord audio sending node · Lavalink v4 compatible";

const GAP: usize = 3;

const STOPS: [(u8, u8, u8); 3] = [(200, 30, 30), (255, 110, 30), (255, 220, 80)];
const ORANGE: (u8, u8, u8) = STOPS[1];

fn fg((r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn gradient_at(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f32;
    let i = (t.floor() as usize).min(STOPS.len() - 2);
    let f = t - i as f32;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn render_logo_line(line: &str, width: usize, color: bool) -> String {
    let len = line.chars().count();
    let pad = " ".repeat(width.saturating_sub(len));
    if !color {
        return format!("{line}{pad}");
    }
    let span = (width.max(2) - 1) as f32;
    let mut out = String::from(BOLD);
    for (i, ch) in line.chars().enumerate() {
        if ch == ' ' {
            out.push(' ');
        } else {
            out.push_str(&fg(gradient_at(i as f32 / span)));
            out.push(ch);
        }
    }
    out.push_str(RESET);
    out.push_str(&pad);
    out
}

fn render_title(text: &str, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let span = (text.chars().count().max(2) - 1) as f32;
    let mut out = String::from(BOLD);
    for (i, ch) in text.chars().enumerate() {
        out.push_str(&fg(gradient_at(i as f32 / span)));
        out.push(ch);
    }
    out.push_str(RESET);
    out
}

pub fn print(cfg: &LoggingConfig) {
    if !cfg.banner || cfg.format == LogFormat::Json {
        return;
    }

    let (accent, dim, reset) =
        if cfg.color { (fg(ORANGE), DIM, RESET) } else { (String::new(), "", "") };

    let logo_lines: Vec<&str> = LOGO.lines().collect();
    let width = logo_lines.iter().map(|l| l.chars().count()).max().unwrap_or(1);

    let cores = std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "?".to_string());

    let commit = env!("KAIRO_GIT_COMMIT");
    let short_commit = commit.get(..7).unwrap_or(commit);

    let rows = [
        ("Version", format!("v{}", env!("CARGO_PKG_VERSION"))),
        ("Build time", env!("KAIRO_BUILD_TIME").to_string()),
        ("Branch", env!("KAIRO_GIT_BRANCH").to_string()),
        ("Commit", short_commit.to_string()),
        ("Rust", env!("KAIRO_RUSTC_VERSION").to_string()),
        ("Cores", cores),
        ("Platform", format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)),
        ("Player", env!("KAIRO_PLAYER_VERSION").to_string()),
        ("Voice", env!("KAIRO_VOICE_VERSION").to_string()),
    ];

    let mut info: Vec<String> = Vec::with_capacity(rows.len() + 2);
    info.push(render_title("Kairo", cfg.color));
    info.push(format!("{dim}{}{reset}", "─".repeat(38)));
    info.extend(
        rows.iter()
            .map(|(label, value)| format!("{accent}▸{reset} {dim}{label:<11}{reset}{value}")),
    );

    let total = logo_lines.len().max(info.len());
    let logo_off = (total - logo_lines.len()) / 2;
    let info_off = (total - info.len()) / 2;
    let gap = " ".repeat(GAP);

    println!();
    for i in 0..total {
        let left = match i.checked_sub(logo_off).and_then(|j| logo_lines.get(j)) {
            Some(l) => render_logo_line(l, width, cfg.color),
            None => " ".repeat(width),
        };
        let right =
            i.checked_sub(info_off).and_then(|j| info.get(j)).map(String::as_str).unwrap_or("");
        println!("{left}{gap}{right}");
    }

    let line_len = TAGLINE.chars().count().max(width);
    println!();
    println!("{dim}{TAGLINE}{reset}");
    println!("{dim}{}{reset}", "─".repeat(line_len));
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_renders_with_and_without_color() {
        let plain = render_title("Kairo", false);
        assert_eq!(plain, "Kairo");

        let colored = render_title("Kairo", true);
        assert!(colored.contains('K'));
        assert!(colored.contains(BOLD));
        assert!(colored.contains(RESET));
    }

    #[test]
    fn banner_prints_without_panic() {
        let plain_cfg = LoggingConfig { banner: true, color: false, ..Default::default() };
        print(&plain_cfg);

        let color_cfg = LoggingConfig { banner: true, color: true, ..Default::default() };
        print(&color_cfg);
    }
}
