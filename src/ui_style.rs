//! UI 设计 token（shadcn/ui 骨架移植到 egui）。
//!
//! 对齐工具是生产力界面，视觉定位「专业、克制、清晰」，与播放器的霓虹炫彩刻意区分：
//! - 语义化配色（surface / border / foreground / muted / primary / warn / success / danger），
//!   所有颜色集中在 [`tokens`] 派生一次，绘制代码不写死 hex；
//! - 单源圆角 [`RADIUS`]，其余按比例派生；
//! - 平面分层用 1px 边框，不靠阴影；阴影只留给浮层（这里不用）；
//! - 状态色克制：低置信用暖黄 `warn` 而非刺眼橙，播放光标用柔和 `danger` 而非纯红。
//!
//! 参考 mp3-visualizer/src/ui_style.rs 的设计语言，按工具场景重新取色。

use eframe::egui::{self, Color32, Rounding, Stroke, Vec2};

/// 单源圆角（对应 --radius: 0.625rem）。
pub const RADIUS: f32 = 6.0;
pub const RADIUS_SM: f32 = RADIUS * 0.6;
pub const RADIUS_MD: f32 = RADIUS * 0.8;
pub const RADIUS_XL: f32 = RADIUS * 1.4;

/// 语义化颜色集合，全部集中在 [`tokens`] 取色。
#[derive(Clone, Copy)]
pub struct UiColors {
    pub surface: Color32,       // 面板底（抬升一层）
    pub surface_alt: Color32,   // 控件底 / 次级表面
    pub surface_sunk: Color32,  // 凹陷画布（时间轴背景）
    pub border: Color32,        // 1px 分层边框
    pub border_strong: Color32, // 强边框（焦点 / 选中描边）
    pub foreground: Color32,    // 正文
    pub muted: Color32,         // 次级文字
    pub subtle: Color32,        // 弱化文字（刻度 / 占位）
    pub primary: Color32,       // 强调色（主操作 / 选中）
    pub primary_soft: Color32,  // hover / 选中底色
    pub success: Color32,       // 高置信 / 成功
    pub warn: Color32,          // 低置信 / 关注
    pub danger: Color32,        // 播放光标 / 危险
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

/// 当前主题的全部语义色（中性深色基底 + 蓝色强调）。
pub fn tokens() -> UiColors {
    let surface = rgb(28, 32, 38);      // #1C2026
    let primary = rgb(94, 156, 255);    // #5E9CFF
    let muted = rgb(143, 153, 170);     // #8F99AA
    UiColors {
        surface,
        surface_alt: rgb(36, 41, 49),   // #242931
        surface_sunk: rgb(17, 20, 25),  // #111419
        border: rgb(52, 59, 70),        // #343B46
        border_strong: rgb(74, 84, 99), // #4A5463
        foreground: rgb(228, 233, 242), // #E4E9F2
        muted,
        subtle: rgb(104, 113, 128),     // #687180
        primary,
        primary_soft: mix(primary, surface, 0.82),
        success: rgb(86, 204, 150),     // #56CC96
        warn: rgb(240, 181, 84),        // #F0B554
        danger: rgb(232, 106, 106),     // #E86A6A
    }
}

/// 每帧把 token 写进 egui 全局样式。
pub fn apply(ctx: &egui::Context) {
    let t = tokens();
    let mut style = (*ctx.style()).clone();

    // 间距：紧凑但透气。
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(11.0, 6.0);
    style.spacing.slider_width = 104.0;
    style.spacing.combo_width = 96.0;
    style.spacing.window_margin = egui::Margin::symmetric(12.0, 10.0);

    style.visuals.dark_mode = true;

    let w = &mut style.visuals.widgets;
    // 非交互：正文用 foreground，弱底 + 1px 边框。
    w.noninteractive.bg_fill = t.surface_alt;
    w.noninteractive.weak_bg_fill = t.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke.color = t.foreground;
    w.noninteractive.rounding = Rounding::same(RADIUS_MD);

    // 静止控件：抬升底 + 1px 边框 + 高对比前景。
    w.inactive.bg_fill = t.surface_alt;
    w.inactive.weak_bg_fill = t.surface_alt;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border);
    w.inactive.fg_stroke.color = t.foreground;
    w.inactive.rounding = Rounding::same(RADIUS_MD);

    // hover：只用浅主题色底，不变形、不发光（克制反馈）。
    w.hovered.bg_fill = t.primary_soft;
    w.hovered.weak_bg_fill = t.primary_soft;
    w.hovered.bg_stroke = Stroke::new(1.0, t.primary.gamma_multiply(0.55));
    w.hovered.fg_stroke.color = t.foreground;
    w.hovered.rounding = Rounding::same(RADIUS_MD);

    // active：主题色填充，主操作语义。
    w.active.bg_fill = t.primary;
    w.active.weak_bg_fill = t.primary;
    w.active.bg_stroke = Stroke::new(1.0, t.primary);
    w.active.fg_stroke.color = rgb(14, 20, 32);
    w.active.rounding = Rounding::same(RADIUS_MD);

    w.open = w.hovered;

    // 选中态：主题色低透明度，不用高饱和整块。
    style.visuals.selection.bg_fill = t.primary.gamma_multiply(0.30);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, t.primary.gamma_multiply(0.75));
    style.visuals.extreme_bg_color = t.surface_sunk;
    style.visuals.faint_bg_color = t.surface_alt;
    style.visuals.code_bg_color = t.surface_sunk;
    style.visuals.window_fill = t.surface;
    style.visuals.window_stroke = Stroke::new(1.0_f32, t.border);
    style.visuals.window_rounding = Rounding::same(RADIUS);
    style.visuals.collapsing_header_frame = true;

    ctx.set_style(style);
}

/// 面板位置，用于决定圆角朝向（贴合窗口边缘的一侧不留圆角）。
#[derive(Clone, Copy)]
pub enum PanelSide {
    Top,
    Bottom,
    Left,
    Central,
}

/// 面板 Frame：surface 底 + 1px 边框 + 单源圆角，无阴影。
pub fn panel_frame(t: &UiColors, side: PanelSide) -> egui::Frame {
    let rounding = match side {
        PanelSide::Top => Rounding {
            nw: 0.0, ne: 0.0, sw: RADIUS, se: RADIUS,
        },
        PanelSide::Bottom => Rounding {
            nw: RADIUS, ne: RADIUS, sw: 0.0, se: 0.0,
        },
        PanelSide::Left => Rounding {
            nw: 0.0, ne: RADIUS, sw: 0.0, se: RADIUS,
        },
        PanelSide::Central => Rounding::same(RADIUS),
    };
    egui::Frame {
        inner_margin: egui::Margin::symmetric(14.0, 9.0),
        outer_margin: egui::Margin::ZERO,
        rounding,
        shadow: egui::Shadow::NONE,
        fill: t.surface,
        stroke: Stroke::new(1.0, t.border),
    }
}

/// 标题左侧的强调色小竖条 + 文本（统一各区标题视觉）。
pub fn section_title(ui: &mut egui::Ui, t: &UiColors, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, 16.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, RADIUS_SM, t.primary);
        ui.add(egui::Label::new(
            egui::RichText::new(text).size(15.0).strong().color(t.foreground),
        ));
    });
}
