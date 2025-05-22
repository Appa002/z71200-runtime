use anyhow::Result;
use anyhow::anyhow;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontWeight, Layout, LayoutContext, StyleProperty,
};
use skia_safe::{Canvas, Font, FontMgr, FontStyle, Paint, TextBlob};
use smallvec::SmallVec;
use std::borrow::Cow;

pub fn layout_text(
    text: &str,
    max_width: f32,
    font_alignment: Alignment,
    font_ctx: &mut FontContext,
    layout_ctx: &mut LayoutContext<()>,
    font_family: &str,
    font_size: f32,
    display_scale: f32,
) -> Layout<()> {
    let mut builder = layout_ctx.ranged_builder(font_ctx, text, display_scale, true);
    builder.push_default(StyleProperty::FontSize(font_size));
    builder.push_default(StyleProperty::FontStack(parley::FontStack::Source(
        Cow::from(font_family),
    )));
    builder.push_default(StyleProperty::FontWeight(FontWeight::NORMAL));
    builder.push_default(StyleProperty::LetterSpacing(0.1));

    let mut layout: Layout<()> = builder.build(&text);
    layout.break_all_lines(Some(max_width));
    layout.align(Some(max_width), font_alignment, AlignmentOptions::default());
    layout
}

pub fn draw_text(
    layout: &Layout<()>,
    x: f32,
    y: f32,
    canvas: &Canvas,
    paint: &Paint,
    font_family: &str,
    font_size: f32,
    display_scale: f32,
) -> Result<()> {
    let fntmgr = FontMgr::new();
    let typeface = fntmgr
        .match_family_style(font_family, FontStyle::normal())
        .ok_or(anyhow!(
            "Could not find font with for family {:?}",
            font_family
        ))?;
    let skia_font = Font::new(typeface, font_size * display_scale);

    let mut paint = paint.clone();
    paint.set_anti_alias(true);

    // let start = std::time::Instant::now();
    for line in layout.lines() {
        for item in line.items() {
            match item {
                parley::PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let mut run_x = glyph_run.offset() + x;
                    let run_y = glyph_run.baseline() + y;

                    // Collect all the glyphs
                    let mut glyph_ids: SmallVec<[skia_safe::GlyphId; 128]> = SmallVec::new();
                    let mut positions: SmallVec<[f32; 128]> = SmallVec::new();

                    for glyph in glyph_run.glyphs() {
                        glyph_ids.push(glyph.id as skia_safe::GlyphId);
                        positions.push(run_x + glyph.x);
                        run_x += glyph.advance;
                    }

                    // Render this run together
                    let blob = TextBlob::from_pos_text_h(
                        &glyph_ids.as_slice(),
                        &positions,
                        run_y,
                        &skia_font,
                    )
                    .ok_or(anyhow!("Coudln't create TextBlob for run."))?;

                    canvas.draw_text_blob(blob, (0.0, 0.0), &paint);
                }

                parley::PositionedLayoutItem::InlineBox(_) => todo!(),
            }
        }
    }
    // println!("elapsed rendering: {:?}", start.elapsed());

    Ok(())
}
