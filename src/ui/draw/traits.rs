use std::time::Duration;

use anyhow::{Result, anyhow};
use skia_safe::Color;
use winit::window::CursorIcon;

use super::utils::read_str_from_array_tagged_word;
use super::{DisplayOption, StoredAlignment, Tag, TaggedWord};

pub(super) trait HasStaticConfig {
    fn file_start(&self) -> *const u8;
    fn base_font_size(&self) -> f32;
    fn display_scale(&self) -> f32;
    #[allow(dead_code)]
    fn get_dt(&self) -> Duration;
}

/* :::::---- Defines the structure of multi tagged word sequences ie how an instruction demands parameters ----::::: */

pub(super) trait ReadIn: Sized + Copy {
    unsafe fn read_in(cursor: &mut *const u8) -> Self {
        let n = std::mem::size_of::<Self>();
        let ptr = (*cursor) as *const Self;
        *cursor = unsafe { cursor.add(n) };
        unsafe { *ptr }
    }
}
impl ReadIn for TaggedWord {}

pub(super) trait HasStack {
    fn stack_pop(&mut self) -> Option<TaggedWord>;
    fn stack_push(&mut self, v: TaggedWord) -> ();
}
pub(super) trait HasRegister {
    fn regs_get(&mut self, k: usize) -> Option<TaggedWord>;
    fn regs_set(&mut self, k: usize, v: TaggedWord) -> ();
}
pub(super) trait HasCursor {
    unsafe fn read_from_cursor(&mut self) -> Option<TaggedWord>;
    unsafe fn peak_cursor(&self) -> Option<TaggedWord>;
}

pub(super) trait Executor<S, C, G>
where
    Self: Intepreter,
    S: HasRegister + HasStack,
    C: HasCursor,
    G: HasStaticConfig,
{
    fn get_config(&self) -> G;
    fn get_cursor(&mut self) -> &mut C;
    fn get_vm_state(&mut self) -> &mut S;

    fn maybe_dereference_from_vm_state(&mut self, tagged_word: TaggedWord) -> Result<TaggedWord> {
        let (tag, word) = match &tagged_word.tag {
            Tag::PullArg => {
                let pulled = &self
                    .get_vm_state()
                    .stack_pop()
                    .ok_or(anyhow!("PullArg called with empty stack."))?;
                (pulled.tag, pulled.word)
            }
            Tag::PullArgOr => {
                /* read the next word, and provide it as the default if nothing is on the stack */
                let default = unsafe { self.get_cursor().read_from_cursor() }
                    .ok_or(anyhow!("Unexpected EoF"))?;

                if let Some(pulled) = &self.get_vm_state().stack_pop() {
                    (pulled.tag, pulled.word)
                } else {
                    (default.tag, default.word)
                }
            }
            Tag::FromReg => {
                let pulled = self
                    .get_vm_state()
                    .regs_get(unsafe { tagged_word.word.word })
                    .ok_or(anyhow!(
                        "FromReg called for register id {}, but it is empty",
                        &unsafe { tagged_word.word.word }
                    ))?;

                (pulled.tag, pulled.word)
            }
            Tag::FromRegOr => {
                /* read the next word, and provide it as the default or pull if reg empty*/
                let default = unsafe { self.get_cursor().read_from_cursor() }
                    .ok_or(anyhow!("Unexpected EoF"))?;
                if let Some(pulled) = self
                    .get_vm_state()
                    .regs_get(unsafe { tagged_word.word.word })
                {
                    (pulled.tag, pulled.word)
                } else {
                    (default.tag, default.word)
                }
            }
            _ => (tagged_word.tag, tagged_word.word),
        };
        Ok(TaggedWord { tag, word })
    }

    unsafe fn read_from_cursor_with_arg(&mut self) -> Result<Option<TaggedWord>> {
        if let Some(tagged_word) = unsafe { self.get_cursor().read_from_cursor() } {
            return Ok(Some(self.maybe_dereference_from_vm_state(tagged_word)?));
        }
        Ok(None)
    }

    fn advance(&mut self, trace: &mut Vec<TaggedWord>) -> Result<Option<()>> {
        let maybe_tagged_word = unsafe { self.get_cursor().read_from_cursor() };
        if let Some(tagged_word) = maybe_tagged_word {
            trace.push(tagged_word);
            match tagged_word.tag {
                Tag::Enter => self.handle_enter()?,
                Tag::Leave => self.handle_leave()?,
                Tag::Rect => self.read_as_rect()?,
                Tag::BeginPath => self.read_as_begin_path()?,
                Tag::Color => self.read_as_pencil_color()?,
                Tag::Width => self.read_as_width()?,
                Tag::Height => self.read_as_height()?,
                Tag::Padding => self.read_as_padding()?,
                Tag::Margin => self.read_as_margin()?,
                Tag::Display => self.handle_display(tagged_word.read_as_display()?)?,
                Tag::RoundedRect => self.read_as_rounded_rect()?,
                Tag::Gap => self.read_as_gap()?,
                Tag::Hover => self.handle_hover(tagged_word.read_as_hover()?)?,
                Tag::MousePressed => {
                    self.handle_mouse_pressed(tagged_word.read_as_mouse_pressed()?)?
                }
                Tag::Clicked => self.handle_clicked(tagged_word.read_as_clicked()?)?,
                Tag::NoJmp => self.handle_no_jmp(tagged_word.read_as_no_jmp()?)?,
                Tag::Jmp => self.handle_jmp(tagged_word.read_as_jmp()?)?,

                Tag::PushArg => self.blanket_handle_push_arg()?,
                Tag::LoadReg => {
                    self.blanket_handle_set_reg(tagged_word.read_as_load_register()?)?
                }
                Tag::Event => self.handle_event(tagged_word.read_as_event()?)?,
                Tag::Text => self.read_as_text()?,
                Tag::FontSize => self.handle_font_size(tagged_word.read_as_font_size()?)?,
                Tag::FontAlignment => {
                    self.handle_font_alignment(tagged_word.read_as_font_alignment()?)?
                }
                Tag::FontFamily => self.read_as_font_family()?,
                Tag::CursorDefault => self.handle_cursor(tagged_word.read_as_any_cursor()?)?,
                Tag::CursorPointer => self.handle_cursor(tagged_word.read_as_any_cursor()?)?,
                _ => {
                    return Err(anyhow!(
                        "Found Tag `{:?}` in illegal position",
                        tagged_word.tag
                    ));
                }
            }
        } else {
            return Ok(None);
        };
        Ok(Some(()))
    }

    fn read_as_width(&mut self) -> Result<()> {
        let width = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        self.handle_width(width)?;
        Ok(())
    }

    fn read_as_height(&mut self) -> Result<()> {
        let height = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        self.handle_height(height)?;
        Ok(())
    }

    fn read_as_margin(&mut self) -> Result<()> {
        let left = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let top = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let right = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let bottom = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        self.handle_margin(left, top, right, bottom)?;
        Ok(())
    }

    fn read_as_padding(&mut self) -> Result<()> {
        let left = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let top = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let right = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let bottom = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        self.handle_padding(left, top, right, bottom)?;
        Ok(())
    }

    fn read_as_gap(&mut self) -> Result<()> {
        let width = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let height = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        self.handle_gap(width, height)?;
        Ok(())
    }

    fn read_as_text(&mut self) -> Result<()> {
        let x = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let y = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;

        let ptr = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_text_ptr()?;
        let txt = read_str_from_array_tagged_word(ptr, self.get_config().file_start())?;
        self.handle_text(x, y, &txt)?;
        Ok(())
    }

    fn read_as_font_family(&mut self) -> Result<()> {
        let ptr = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_text_ptr()?;
        let txt = read_str_from_array_tagged_word(ptr, self.get_config().file_start())?;
        self.handle_font_family(&txt)?;
        Ok(())
    }

    fn read_as_rect(&mut self) -> Result<()> {
        let x = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let y = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;

        let w = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let h = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        self.handle_rect(x, y, w, h)?;
        Ok(())
    }

    fn read_as_rounded_rect(&mut self) -> Result<()> {
        let x = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let y = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pct(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let w = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let h = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;
        let r = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_taffy_length_pctauto(
                self.get_config().base_font_size(),
                self.get_config().display_scale(),
            )?;

        self.handle_rounded_rect(x, y, w, h, r)?;
        Ok(())
    }

    fn read_as_pencil_color(&mut self) -> Result<()> {
        let color = unsafe { self.read_from_cursor_with_arg() }?
            .ok_or(anyhow!("Early EOF"))?
            .read_as_any_color()?;
        self.handle_pencil_color(color)?;
        Ok(())
    }

    fn read_as_begin_path(&mut self) -> Result<()> {
        self.handle_begin_path()?;
        while let Some(tagged_word) = unsafe { self.get_cursor().read_from_cursor() } {
            match tagged_word.tag {
                Tag::BeginPath => return Err(anyhow!("Nested paths are forbidden.")),
                Tag::EndPath => break,
                Tag::MoveTo => {
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    self.handle_move_to(x, y)?;
                }
                Tag::LineTo => {
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    self.handle_line_to(x, y)?;
                }
                Tag::QuadTo => {
                    let cx = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let cy = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    self.handle_quad_to(cx, cy, x, y)?;
                }
                Tag::CubicTo => {
                    let cx1 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let cy1 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let cx2 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let cy2 = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    self.handle_cubic_to(cx1, cy1, cx2, cy2, x, y)?;
                }
                Tag::ArcTo => {
                    let tx = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let ty = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let x = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let y = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    let r = unsafe { self.read_from_cursor_with_arg() }?
                        .ok_or(anyhow!("Early EOF"))?
                        .read_as_taffy_length_pct(
                            self.get_config().base_font_size(),
                            self.get_config().display_scale(),
                        )?;
                    self.handle_arc_to(tx, ty, x, y, r)?;
                }
                Tag::ClosePath => self.handle_close_path()?,
                _ => {
                    return Err(anyhow!(
                        "Expected only tags of Path family after `BeginPath`"
                    ));
                }
            }
        }

        if unsafe { self.get_cursor().peak_cursor().map(|x| x.tag) } != Some(Tag::EndPath) {
            return Err(anyhow!(
                "A path was opened with `BeginPath` but was never closed with `EndPath`"
            ));
        }
        self.handle_end_path()?;
        Ok(())
    }

    fn blanket_handle_push_arg(&mut self) -> Result<()> {
        let tagged_word =
            unsafe { self.get_cursor().read_from_cursor() }.ok_or(anyhow!("Unexpected EOF"))?;
        let tagged_word = self.maybe_dereference_from_vm_state(tagged_word)?;
        self.get_vm_state().stack_push(tagged_word);
        Ok(())
    }

    fn blanket_handle_set_reg(&mut self, id: usize) -> Result<()> {
        let tagged_word =
            unsafe { self.get_cursor().read_from_cursor() }.ok_or(anyhow!("Unexpected EOF"))?;
        let tagged_word = self.maybe_dereference_from_vm_state(tagged_word)?;
        self.get_vm_state().regs_set(id, tagged_word);
        Ok(())
    }
}

pub(super) trait Intepreter {
    fn handle_enter(&mut self) -> Result<()> {
        Ok(())
    }
    fn handle_leave(&mut self) -> Result<()> {
        Ok(())
    }
    fn handle_width(&mut self, _x: taffy::LengthPercentageAuto) -> Result<()> {
        Ok(())
    }
    fn handle_height(&mut self, _y: taffy::LengthPercentageAuto) -> Result<()> {
        Ok(())
    }
    fn handle_margin(
        &mut self,
        _left: taffy::LengthPercentageAuto,
        _top: taffy::LengthPercentageAuto,
        _right: taffy::LengthPercentageAuto,
        _bottom: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_padding(
        &mut self,
        _left: taffy::LengthPercentage,
        _top: taffy::LengthPercentage,
        _right: taffy::LengthPercentage,
        _bottom: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_display(&mut self, _display: DisplayOption) -> Result<()> {
        Ok(())
    }
    fn handle_gap(
        &mut self,
        _width: taffy::LengthPercentage,
        _height: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_hover(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_mouse_pressed(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_clicked(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_no_jmp(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_jmp(&mut self, _rel_ptr: usize) -> Result<()> {
        Ok(())
    }
    fn handle_text(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _txt: &str,
    ) -> Result<()> {
        Ok(())
    }
    fn handle_font_alignment(&mut self, _alignment: StoredAlignment) -> Result<()> {
        Ok(())
    }
    fn handle_font_family(&mut self, _font_desc: &str) -> Result<()> {
        Ok(())
    }
    fn handle_font_size(&mut self, _size: f32) -> Result<()> {
        Ok(())
    }

    fn handle_rect(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _w: taffy::LengthPercentageAuto,
        _h: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_pencil_color(&mut self, _color: Color) -> Result<()> {
        Ok(())
    }

    fn handle_cursor(&mut self, _cursor: CursorIcon) -> Result<()> {
        Ok(())
    }

    fn handle_event(&mut self, _id: usize) -> Result<()> {
        Ok(())
    }

    fn handle_begin_path(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_move_to(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_line_to(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_quad_to(
        &mut self,
        _cx: taffy::LengthPercentage,
        _cy: taffy::LengthPercentage,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_cubic_to(
        &mut self,
        _cx1: taffy::LengthPercentage,
        _cy1: taffy::LengthPercentage,
        _cx2: taffy::LengthPercentage,
        _cy2: taffy::LengthPercentage,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_arc_to(
        &mut self,
        _tx: taffy::LengthPercentage,
        _ty: taffy::LengthPercentage,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _r: taffy::LengthPercentage,
    ) -> Result<()> {
        Ok(())
    }

    fn handle_close_path(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_end_path(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_rounded_rect(
        &mut self,
        _x: taffy::LengthPercentage,
        _y: taffy::LengthPercentage,
        _width: taffy::LengthPercentageAuto,
        _height: taffy::LengthPercentageAuto,
        _r: taffy::LengthPercentageAuto,
    ) -> Result<()> {
        Ok(())
    }
}
