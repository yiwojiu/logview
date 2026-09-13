//! 验证中文字体是否真的被 egui 加载成功。
//! 运行：cargo run --example fontcheck
//! 若 before == after，说明字体没生效，界面上中文会是方框。

use eframe::egui;

fn glyph_width(ctx: &egui::Context) -> f32 {
    ctx.fonts(|f| f.glyph_width(&egui::FontId::proportional(14.0), '中'))
}

fn main() {
    let ctx = egui::Context::default();

    // egui 的字体在首次 run 之前不可用，所以探测必须发生在 run 内部
    let mut before = 0.0f32;
    let _ = ctx.run(Default::default(), |ctx| before = glyph_width(ctx));

    logview::fonts::setup(&ctx);

    let mut after = 0.0f32;
    let _ = ctx.run(Default::default(), |ctx| after = glyph_width(ctx));

    println!("'中' 字形宽度：加载前 {before:.2} → 加载后 {after:.2}");
    if (before - after).abs() < 0.01 {
        println!("结果：未生效，需手动指定 CJK 字体路径");
        std::process::exit(1);
    }
    println!("结果：中文字体已加载");
}
