// use std::{collections::HashMap, num::ParseIntError, str::FromStr};

// use anyhow::{Context, Result, anyhow};

// use super::draw::{ParamUnion, Tag};

// #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
// enum Modifier {
//     F,
//     W,
//     B,
// }

// struct MacroRhs {
//     bytes: Vec<u8>,
//     kind: Modifier,
// }
// impl MacroRhs {
//     // Convert MacroRhs to f32 based on the kind
//     fn as_f32(&self) -> Result<f32> {
//         match self.kind {
//             Modifier::F => {
//                 // For F kind, interpret bytes as f32
//                 if self.bytes.len() >= 4 {
//                     // Create a properly sized array to store the bytes
//                     let mut f32_bytes = [0u8; 4];
//                     // Copy up to 4 bytes from the Vec<u8> into the array
//                     for (i, &byte) in self.bytes.iter().take(4).enumerate() {
//                         f32_bytes[i] = byte;
//                     }
//                     // Interpret the bytes as f32
//                     Ok(f32::from_le_bytes(f32_bytes))
//                 } else {
//                     // If there aren't enough bytes, return 0.0
//                     Ok(0.0)
//                 }
//             }
//             Modifier::W => {
//                 // For W kind, interpret bytes as usize and then cast to f32
//                 if self.bytes.len() >= std::mem::size_of::<usize>() {
//                     // Create a properly sized array to store the bytes
//                     let mut usize_bytes = [0u8; std::mem::size_of::<usize>()];
//                     // Copy bytes from the Vec<u8> into the array
//                     for (i, &byte) in self
//                         .bytes
//                         .iter()
//                         .take(std::mem::size_of::<usize>())
//                         .enumerate()
//                     {
//                         usize_bytes[i] = byte;
//                     }
//                     // Interpret the bytes as usize
//                     let value = usize::from_le_bytes(usize_bytes);
//                     Ok(value as f32)
//                 } else {
//                     // If there aren't enough bytes, return 0.0
//                     Ok(0.0)
//                 }
//             }
//             _ => {
//                 return Err(anyhow!("Can't inteprete b word kind as f32"));
//             }
//         }
//     }
// }

// fn parse_hex_bytes(hex_str: &str) -> Result<(u8, u8, u8, u8)> {
//     if hex_str.len() != 6 && hex_str.len() != 8 {
//         return Err(anyhow!(
//             "Invalid hex length: {}, expected 6 or 8 characters",
//             hex_str.len()
//         ));
//     }

//     // Validate hex characters
//     if !hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
//         return Err(anyhow!("Invalid hex characters".to_string()));
//     }

//     // Parse two chars at a time into bytes
//     let parse_byte = |idx: usize| -> Result<u8, ParseIntError> {
//         let start = idx * 2;
//         let end = start + 2;
//         if end <= hex_str.len() {
//             u8::from_str_radix(&hex_str[start..end], 16)
//         } else {
//             Ok(0) // Default to 0 if byte is missing
//         }
//     };

//     // Parse all bytes
//     let r = parse_byte(0).map_err(|e| anyhow!("Failed to parse R component: {}", e))?;
//     let g = parse_byte(1).map_err(|e| anyhow!("Failed to parse G component: {}", e))?;
//     let b = parse_byte(2).map_err(|e| anyhow!("Failed to parse B component: {}", e))?;

//     // Handle alpha component - use 0 if not present (for 6-char hex)
//     let a = if hex_str.len() >= 8 {
//         parse_byte(3).map_err(|e| anyhow!("Failed to parse A component: {}", e))?
//     } else {
//         0
//     };

//     Ok((r, g, b, a))
// }

// fn split_or_whole<'a>(input: &'a str, pattern: &str) -> (&'a str, Option<&'a str>) {
//     if let Some((left, right)) = input.split_once(pattern) {
//         (left, Some(right))
//     } else {
//         (input, None)
//     }
// }

// fn parse_modifier(word_str: &str) -> Result<Modifier> {
//     let modifier = word_str
//         .chars()
//         .nth(0)
//         .ok_or(anyhow!("word must start with 'w',  'f', or 'b'"))?;
//     match modifier {
//         'w' => Ok(Modifier::W),
//         'f' => Ok(Modifier::F),
//         'b' => Ok(Modifier::B),
//         _ => Err(anyhow!("word kind must be one of 'w',  'f', or 'b'")),
//     }
// }

// fn word_str_as_literal(word_str: &str) -> Result<Vec<u8>> {
//     let modifier = parse_modifier(word_str)?;

//     let value = &word_str[1..];
//     let union = match modifier {
//         Modifier::F => ParamUnion {
//             real: f32::from_str(value)?,
//         },
//         Modifier::W => ParamUnion {
//             word: usize::from_str(value)?,
//         },
//         Modifier::B => ParamUnion {
//             long_color: parse_hex_bytes(value)?,
//         },
//     };

//     let bytes = unsafe {
//         std::slice::from_raw_parts(
//             (&union as *const ParamUnion) as *const u8,
//             size_of::<usize>(),
//         )
//     };

//     Ok(bytes.to_vec())
// }

// fn parse_word_str(word_str: &str, macros: &HashMap<String, MacroRhs>) -> Result<Vec<u8>> {
//     let bytes = if word_str.chars().nth(0) == Some('@') {
//         &macros
//             .get(&word_str[1..])
//             .ok_or(anyhow!("Unknown macro with name '{}'", &word_str[1..]))?
//             .bytes
//     } else if word_str.chars().nth(1) == Some('{') {
//         let modifier = word_str.chars().nth(0).ok_or(anyhow!(
//             "Literal expression has to be annotated with type like 'f{{...}}'"
//         ))?;
//         let expr = &word_str[2..word_str.len() - 1];

//         // create a "context" with all the macros
//         let mut ctx = meval::Context::new();
//         for (key, value) in macros.iter() {
//             if value.kind == Modifier::B {
//                 continue;
//             }
//             ctx.var(key, value.as_f32()? as f64);
//         }

//         if modifier == 'w' {
//             &unsafe {
//                 ParamUnion {
//                     word: meval::eval_str_with_context(expr, ctx)? as usize,
//                 }
//                 ._debug_bytes
//                 .to_vec()
//             }
//         } else if modifier == 'f' {
//             &unsafe {
//                 ParamUnion {
//                     real: meval::eval_str_with_context(expr, ctx)? as f32,
//                 }
//                 ._debug_bytes
//                 .to_vec()
//             }
//         } else if modifier == 'b' {
//             return Err(anyhow!(
//                 "'b' is a valid word modifier, but cannot be used with ad-hoc expressions"
//             ));
//         } else {
//             return Err(anyhow!("modifier must be start with 'w',  'f', or 'b'"));
//         }
//     } else {
//         &word_str_as_literal(word_str)?.to_owned()
//     };

//     Ok(bytes.to_owned())
// }

// fn assemble_inst_line(
//     code_str: &str,
//     out: &mut Vec<u8>,
//     macros: &mut HashMap<String, MacroRhs>,
// ) -> Result<()> {
//     let (tag_str, word_str) = split_or_whole(code_str, ",");
//     let tag_str = tag_str.trim();
//     let word_str = word_str.map(|x| x.trim());
//     if tag_str.is_empty() {
//         return Ok(());
//     }

//     let tag = Tag::from_str(tag_str).with_context(|| format!("tag string is `{}`", tag_str))?;

//     out.extend_from_slice(&(tag as usize).to_le_bytes());
//     if let Some(word_str) = word_str {
//         let bytes = parse_word_str(word_str, macros)?;

//         for b in bytes {
//             out.push(b);
//         }
//     } else {
//         /* if there is no word we still need to write some bytes to keep the structure intact */
//         for _ in 0..size_of::<usize>() {
//             out.push(0xAA);
//         }
//     }
//     Ok(())
// }

// fn assemble_macro_line(code_str: &str, macros: &mut HashMap<String, MacroRhs>) -> Result<()> {
//     let (tagged_name, word_str) = code_str.split_once("=").ok_or(anyhow!(
//         "Macro line syntax is: @<name> = <word>, found {:?}",
//         code_str
//     ))?;
//     let tagged_name = tagged_name.trim();
//     let word_str = word_str.trim();

//     if tagged_name.len() <= 1 {
//         return Err(anyhow!(
//             "Macro line syntax is: @<name> = <word>, found {:?}",
//             code_str
//         ));
//     }

//     let name = &tagged_name[1..];

//     if macros.contains_key(name) {
//         return Err(anyhow!("Duplicate macro with name \"{}\"", name));
//     }

//     macros.insert(
//         String::from(name),
//         MacroRhs {
//             bytes: parse_word_str(word_str, macros)?,
//             kind: parse_modifier(word_str)?,
//         },
//     );
//     Ok(())
// }

// pub fn assemble(src: &str) -> Result<Vec<u8>> {
//     let mut out: Vec<u8> = Vec::new();
//     let mut macros: HashMap<String, MacroRhs> = HashMap::new();

//     for (i, line) in src.lines().enumerate() {
//         let (code_str, _) = split_or_whole(line, ";");
//         let code_str = code_str.trim();
//         let first_char = if let Some(c) = code_str.chars().nth(0) {
//             c
//         } else {
//             continue;
//         };

//         if first_char == '@' {
//             assemble_macro_line(code_str, &mut macros)
//                 .with_context(|| format!("in line {}:\n\"{}\"", i, line))?;
//         } else {
//             assemble_inst_line(code_str, &mut out, &mut macros)
//                 .with_context(|| format!("in line {}:\n\"{}\"", i, line))?;
//         }
//     }

//     Ok(out)
// }
