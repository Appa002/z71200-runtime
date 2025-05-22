// use anyhow::Result;
// use std::{collections::HashMap, fmt::Write};

// use super::draw::{ReadFromMemory, Tag, TaggedWord, read_str_from_array_tagged_word};

// fn bytes_to_hex_str(bytes: &[u8]) -> Result<String> {
//     let mut out = String::new();
//     for b in bytes {
//         write!(out, "{:x} ", b)?;
//     }
//     Ok(out)
// }

// fn rotating_color(i: usize) -> String {
//     // ANSI color codes: 31-36 (avoiding 30 for black and 37 for white)
//     // Map i % 6 to the colors 31 through 36
//     let color_code = 31 + (i % 6);
//     format!("\x1b[{}m", color_code)
// }

// fn write_instruction(
//     cursor: *const u8,
//     tag: &TaggedWord,
//     depth: usize,
//     library_depth: usize,
//     branch_ends: &mut Vec<*const u8>,
// ) -> Result<String> {
//     let mut out = String::new();

//     write!(
//         out,
//         "{}{}{:?}, {}",
//         " ".repeat(depth * 4),
//         " |-".repeat(library_depth),
//         tag.tag,
//         bytes_to_hex_str(&unsafe { tag.word._debug_bytes })?
//     )?;

//     let branch_depth = branch_ends
//         .iter()
//         .filter(|end_ptr| **end_ptr >= cursor)
//         .count();

//     if branch_depth > 0 {
//         let stars = (1..=branch_depth)
//             .map(|i| format!("{}*", rotating_color(i)))
//             .collect::<Vec<_>>()
//             .join("");

//         let text_color = rotating_color(branch_depth);
//         out = format!("{}{}{}\x1b[0m", text_color, stars, out);
//     }

//     branch_ends.retain(|end_ptr| *end_ptr > cursor);

//     Ok(out)
// }

// fn trace_to(
//     cursor: &mut *const u8,
//     file_start: *const u8,
//     end: Option<*const u8>,
//     library: &HashMap<usize, Vec<u8>>,
// ) -> Result<String> {
//     let mut out = String::new();

//     let mut call_stack: Vec<*const u8> = Vec::new();
//     let mut depth = 0;
//     let mut library_depth = 0;
//     let mut branch_ends: Vec<*const u8> = Vec::new();

//     loop {
//         // Check alignment first.
//         if (*cursor as usize) % size_of::<usize>() != 0 {
//             writeln!(out, "##### cursor has become unaligned here")?;
//             return Ok(out);
//         }

//         // read the tag
//         let tag = unsafe { TaggedWord::read_in(cursor, &mut Vec::new()) };

//         // pprint it
//         if tag.tag != Tag::Leave {
//             writeln!(
//                 out,
//                 "{}",
//                 write_instruction(*cursor, &tag, depth, library_depth, &mut branch_ends)?
//             )?;
//             /* leave is special because it needs to be move out to match the enter. */
//         }
//         // do the jmps
//         if tag.read_as_enter(None).is_ok() {
//             depth += 1;
//             // new range: (cur_start_ptr, cursor)
//             // cur_start_ptr = unsafe { cursor.sub(2 * size_of::<usize>()) };
//         } else if tag.read_as_leave(None).is_ok() {
//             depth -= 1;
//             writeln!(
//                 out,
//                 "{}",
//                 write_instruction(*cursor, &tag, depth, library_depth, &mut branch_ends)?
//             )?;

//             // new range: (cur_start_ptr, cursor)
//             // cur_start_ptr = cursor;
//         } else if let Ok(id) = tag.read_as_library_call(None) {
//             // branch into the library code.
//             let code = library.get(&id);
//             if let Some(code) = code {
//                 library_depth += 1;
//                 call_stack.push(*cursor);
//                 *cursor = code.as_ptr();
//                 // cur_start_ptr = cursor;
//             } else {
//                 writeln!(out, "##### library with id {} not found here", id)?;
//                 return Ok(out);
//             }
//         } else if tag.read_as_return(None).is_ok() {
//             let ret_ptr = call_stack.pop();
//             if let Some(ret_ptr) = ret_ptr {
//                 library_depth -= 1;
//                 *cursor = ret_ptr;
//             } else {
//                 writeln!(
//                     out,
//                     "##### Return called here but there is no matching previous *Call",
//                 )?;
//                 return Ok(out);
//             }
//         }
//         // Simulate any branches in print.
//         else if let Ok(rel_ptr) = tag.read_as_hover(None) {
//             branch_ends.push(unsafe { cursor.add(rel_ptr) });
//         } else if let Ok(rel_ptr) = tag.read_as_pressed(None) {
//             branch_ends.push(unsafe { cursor.add(rel_ptr) });
//         } else if let Ok(rel_ptr) = tag.read_as_clicked(None) {
//             branch_ends.push(unsafe { cursor.add(rel_ptr) });
//         } else if let Ok(rel_ptr) = tag.read_as_open_latch(None) {
//             branch_ends.push(unsafe { cursor.add(rel_ptr) });
//         } else if let Ok(rel_ptr) = tag.read_as_close_latch(None) {
//             branch_ends.push(unsafe { cursor.add(rel_ptr) });
//         } else if tag.read_as_text(None).is_ok() {
//             let _x = unsafe { TaggedWord::read_in(cursor, &mut Vec::new()) };
//             let _y = unsafe { TaggedWord::read_in(cursor, &mut Vec::new()) };

//             let ptr =
//                 unsafe { TaggedWord::read_in(cursor, &mut Vec::new()) }.read_as_text_ptr(None)?;
//             let _txt = read_str_from_array_tagged_word(ptr, file_start)?;
//         } else if tag.read_as_font_family(None).is_ok() {
//             let ptr =
//                 unsafe { TaggedWord::read_in(cursor, &mut Vec::new()) }.read_as_text_ptr(None)?;
//             let _font_family = read_str_from_array_tagged_word(ptr, file_start)?;
//         }

//         // exit (2 different conditions depending on if bounded)
//         if let Some(end) = end {
//             // bounded case
//             if *cursor >= end {
//                 break;
//             }
//         } else {
//             // unbounded case
//             if depth <= 0 {
//                 break;
//             }
//         }
//     }
//     Ok(out)
// }

// pub fn debug_print_layout(
//     loc: usize,
//     file_start: *const u8,
//     library: &HashMap<usize, Vec<u8>>,
// ) -> Result<String> {
//     let mut out = String::new();
//     writeln!(out, ">>>> Begin Layout Dump")?;
//     let mut cursor = unsafe { file_start.add(loc) };
//     writeln!(out, "{}", trace_to(&mut cursor, file_start, None, library)?)?;
//     Ok(out)
// }
