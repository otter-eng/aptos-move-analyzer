// Copyright (c) The BitsLab.MoveBit Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{analyzer_handler::*, context::*, utils::path_concat};
use codespan::Span;
use lsp_server::*;
use lsp_types::*;
use move_compiler::parser::lexer::Tok;
use move_model::{
    ast::{ExpData::*, Operation::*, Pattern, SpecBlockTarget},
    model::{FunId, GlobalEnv, ModuleId, StructId},
    ty::TypeDisplayContext,
};
use std::path::{Path, PathBuf};

/// Handles on_hover_request of the language server.
pub fn on_hover_request(context: &Context, request: &Request) -> lsp_server::Response {
    log::info!("on_hover_request request = {:?}", request);
    let parameters = serde_json::from_value::<HoverParams>(request.params.clone())
        .expect("could not deserialize go-to-def request");
    let fpath = parameters
        .text_document_position_params
        .text_document
        .uri
        .to_file_path()
        .unwrap();
    let loc = parameters.text_document_position_params.position;
    let line = loc.line;
    let col = loc.character;
    log::info!("LSP coordinates - line: {}, col: {}", line, col);
    let fpath = path_concat(std::env::current_dir().unwrap().as_path(), fpath.as_path());

    let mut handler = Handler::new(fpath.clone(), line, col);
    log::info!(
        "Handler created with line: {}, col: {}",
        handler.line,
        handler.col
    );
    match context.projects.get_project(&fpath) {
        Some(x) => x,
        None => {
            log::error!("project not found:{:?}", fpath.as_path());
            return Response {
                id: "".to_string().into(),
                result: Some(serde_json::json!({"msg": "No available project"})),
                error: None,
            };
        }
    }
    .run_visitor_for_file(&mut handler, &fpath, String::default());

    let r = Response::new_ok(
        request.id.clone(),
        serde_json::to_value(handler.get_result()).unwrap(),
    );
    let ret_response = r.clone();
    log::info!(
        "------------------------------------\n<on_hover>ret_response = \n{:?}\n\n",
        ret_response
    );
    context
        .connection
        .sender
        .send(Message::Response(r))
        .unwrap();
    ret_response
}

pub(crate) struct Handler {
    /// The file we are looking for.
    pub(crate) filepath: PathBuf,
    pub(crate) line: u32,
    pub(crate) col: u32,

    pub(crate) mouse_span: codespan::Span,
    pub(crate) capture_items_span: Vec<codespan::Span>,
    pub(crate) result_candidates: Vec<String>,
    pub(crate) target_module_id: ModuleId,
    pub(crate) target_function_id: Option<FunId>,
}

impl Handler {
    pub(crate) fn new(filepath: impl Into<PathBuf>, line: u32, col: u32) -> Self {
        Self {
            filepath: filepath.into(),
            line,
            col,
            mouse_span: Default::default(),
            capture_items_span: vec![],
            result_candidates: vec![],
            target_module_id: ModuleId::new(0),
            target_function_id: None,
        }
    }

    fn check_move_model_loc_contains_mouse_pos(
        &self,
        env: &GlobalEnv,
        loc: &move_model::model::Loc,
    ) -> bool {
        log::info!(
            "Checking if mouse pos (line: {}, col: {}) is within loc: {:?}",
            self.line,
            self.col,
            loc
        );

        if let Some(obj_first_col) = env.get_location(&move_model::model::Loc::new(
            loc.file_id(),
            codespan::Span::new(
                loc.span().start(),
                loc.span().start() + codespan::ByteOffset(1),
            ),
        )) {
            if let Some(obj_last_col) = env.get_location(&move_model::model::Loc::new(
                loc.file_id(),
                codespan::Span::new(loc.span().end(), loc.span().end() + codespan::ByteOffset(1)),
            )) {
                log::info!(
                    "Location spans from line: {}, col: {} to line: {}, col: {}",
                    u32::from(obj_first_col.line),
                    u32::from(obj_first_col.column),
                    u32::from(obj_last_col.line),
                    u32::from(obj_last_col.column)
                );

                if u32::from(obj_first_col.line) == self.line
                    && u32::from(obj_first_col.column) < self.col
                    && self.col < u32::from(obj_last_col.column)
                {
                    log::info!("Mouse position is within location span!");
                    return true;
                }
            }
        }
        log::info!("Mouse position is NOT within location span");
        false
    }

    fn get_result(&mut self) -> Hover {
        let mut most_clost_item_idx: usize = 0;
        if let Some(item_idx) = find_smallest_length_index(&self.capture_items_span) {
            most_clost_item_idx = item_idx;
        }

        if !self.result_candidates.is_empty() && most_clost_item_idx < self.result_candidates.len()
        {
            let ret_str = self.result_candidates[most_clost_item_idx].clone();
            self.capture_items_span.clear();
            self.result_candidates.clear();
            let hover = Hover {
                contents: HoverContents::Scalar(MarkedString::String(ret_str)),
                range: None,
            };
            return hover;
        }
        self.capture_items_span.clear();
        self.result_candidates.clear();

        Hover {
            contents: HoverContents::Scalar(MarkedString::String("null".to_string())),
            range: None,
        }
    }

    fn get_mouse_loc(&mut self, env: &GlobalEnv, target_fn_or_struct_loc: &move_model::model::Loc) {
        let file_source = env.get_file_source(target_fn_or_struct_loc.file_id());
        let file_index = line_index::LineIndex::new(file_source);

        if let Some(line_offset_start) = file_index.line(self.line) {
            if let Some(line_offset_end) = file_index.offset(line_index::LineCol {
                line: self.line,
                col: self.col,
            }) {
                let mouse_source = env.get_source(&move_model::model::Loc::new(
                    target_fn_or_struct_loc.file_id(),
                    codespan::Span::new(
                        u32::from(line_offset_start.start()),
                        u32::from(line_offset_end),
                    ),
                ));
                log::info!("after get mouse_source = {:?}", mouse_source);
                self.mouse_span = codespan::Span::new(
                    u32::from(line_offset_start.start()),
                    u32::from(line_offset_end),
                );
            }
        }
    }

    fn process_use_decl(&mut self, env: &GlobalEnv) {
        let target_module = env.get_module(self.target_module_id);
        let spool = env.symbol_pool();
        let mut ref_module = String::default();
        let mut target_stct_or_fn = String::default();
        let mut found_target_stct_or_fn = false;
        let mut capture_items_loc = move_model::model::Loc::default();
        let mut is_module_hover = false;
        let mut full_module_name_for_display = String::default();

        log::info!("Processing use declarations for line {}", self.line);
        log::info!("Target module: {}", target_module.get_name().display(env));

        // info: Show the file content around the hover position
        let file_source = env.get_file_source(target_module.get_loc().file_id());
        let lines: Vec<&str> = file_source.lines().collect();
        if (self.line as usize) < lines.len() {
            log::info!(
                "File content at line {}: '{}'",
                self.line,
                lines[self.line as usize]
            );
            log::info!("Hover coordinates: line={}, col={}", self.line, self.col);
            log::info!(
                "Character at hover position: '{}'",
                if (self.col as usize) < lines[self.line as usize].len() {
                    lines[self.line as usize]
                        .chars()
                        .nth(self.col as usize)
                        .unwrap_or(' ')
                } else {
                    ' '
                }
            );
            if self.line > 0 && (self.line as usize - 1) < lines.len() {
                log::info!(
                    "File content at line {}: '{}'",
                    self.line - 1,
                    lines[self.line as usize - 1]
                );
            }
            if (self.line as usize + 1) < lines.len() {
                log::info!(
                    "File content at line {}: '{}'",
                    self.line + 1,
                    lines[self.line as usize + 1]
                );
            }
        }

        for use_decl in target_module.get_use_decls() {
            log::info!("Checking use_decl: {:?}", use_decl);
            log::info!("Use declaration details:");
            log::info!(
                "  - Module name: {}",
                use_decl.module_name.display_full(env)
            );
            log::info!("  - Has members: {}", !use_decl.members.is_empty());
            log::info!("  - Member count: {}", use_decl.members.len());
            if !use_decl.members.is_empty() {
                log::info!(
                    "  - Members: {:?}",
                    use_decl
                        .members
                        .iter()
                        .map(|(_, name, _)| name.display(spool).to_string())
                        .collect::<Vec<_>>()
                );
            }
            log::info!("  - Location: {:?}", env.get_location(&use_decl.loc));

            if let Some(use_pos) = env.get_location(&use_decl.loc) {
                log::info!("Use decl at line: {}", u32::from(use_pos.line));
                log::info!(
                    "Use decl line comparison: {} vs {} (diff: {})",
                    u32::from(use_pos.line),
                    self.line,
                    u32::from(use_pos.line) as i32 - self.line as i32
                );
                if u32::from(use_pos.line) != self.line {
                    // Try adjusting for coordinate system difference
                    if u32::from(use_pos.line) == self.line + 1 {
                        log::info!(
                            "Coordinate system mismatch detected! LSP uses 0-based, move-model uses 1-based"
                        );
                    } else if u32::from(use_pos.line) + 1 == self.line {
                        log::info!(
                            "Coordinate system mismatch detected! LSP uses 1-based, move-model uses 0-based"
                        );
                    }
                    continue;
                }
            }

            log::trace!(
                "use_decl.loc = {:?}, use_decl.loc.len = {:?}",
                use_decl.loc,
                use_decl.loc.span().end() - use_decl.loc.span().start()
            );

            if !use_decl.members.is_empty() {
                log::info!("Use decl has {} members", use_decl.members.len());
                // Check if we're hovering on the module name vs specific items
                // We need to be more precise about the position
                let full_module_name = use_decl.module_name.display_full(env).to_string();

                // Try to find the opening brace position to determine if we're on module name or items
                let line_content = if (self.line as usize) < lines.len() {
                    lines[self.line as usize]
                } else {
                    ""
                };
                log::info!("Line content: '{}'", line_content);

                if let Some(brace_pos) = line_content.find('{') {
                    // Case 1: Use declaration with braces like "use aptos_framework::timestamp::{...}"
                    // Check if we're hovering before the opening brace (module name) or after it (items)
                    let char_pos = self.col as usize;
                    log::info!(
                        "Hover at char position: {}, opening brace at: {}",
                        char_pos,
                        brace_pos
                    );

                    log::info!(
                        "Checking all members first to see if we're hovering on any of them"
                    );
                    for (member_loc, name, _) in use_decl.members.clone().into_iter() {
                        let member_name_str = name.display(spool).to_string();

                        log::info!(
                            "Checking member: {} at location: {:?}",
                            member_name_str,
                            env.get_location(&member_loc)
                        );

                        // Directly check if mouse position is within member span - no line number dependency
                        if self.check_move_model_loc_contains_mouse_pos(env, &member_loc) {
                            log::info!(
                                "Mouse position is within member span for: {}",
                                member_name_str
                            );
                            target_stct_or_fn = member_name_str.clone();
                            found_target_stct_or_fn = true;
                            // For item hover, we need the full module path, not just the short name
                            ref_module = full_module_name.clone();
                            full_module_name_for_display = full_module_name.clone();
                            capture_items_loc = member_loc;
                            is_module_hover = false;
                            log::info!(
                                "Found member: {} in module: {}",
                                member_name_str,
                                full_module_name
                            );
                            break;
                        } else {
                            log::info!(
                                "Mouse position is NOT within member span for: {}",
                                member_name_str
                            );
                        }
                    }

                    // If no member found, then check if hovering on module name vs keywords
                    if !found_target_stct_or_fn {
                        if char_pos <= brace_pos {
                            // Hovering on module name (before the opening brace)
                            log::info!("Hovering on module name in use declaration with braces");
                            full_module_name_for_display = full_module_name.clone();
                            let short_module_name =
                                if let Some(last_part) = full_module_name.split("::").last() {
                                    last_part.to_string()
                                } else {
                                    full_module_name.clone()
                                };
                            ref_module = short_module_name.clone();
                            target_stct_or_fn = short_module_name.clone();
                            found_target_stct_or_fn = true;
                            is_module_hover = true;
                            capture_items_loc = use_decl.loc.clone();
                            log::info!(
                                "Found use declaration on module name - module: {}, item: {}",
                                full_module_name,
                                target_stct_or_fn
                            );
                        } else {
                            // Hovering after opening brace, check for keywords
                            log::info!("Hovering after opening brace, checking for keywords");

                            // ENHANCED: Scan multiple lines to find all keywords until closing brace
                            let mut current_line = self.line as usize;
                            let mut found_closing_brace = false;
                            let mut scanned_content = String::new();

                            // Start with the current line content after the opening brace
                            let line_after_brace = &line_content[brace_pos + 1..];
                            scanned_content.push_str(line_after_brace);

                            // Scan next few lines until we find the closing brace
                            while current_line < lines.len() && !found_closing_brace {
                                if let Some(closing_brace_pos) = lines[current_line].find('}') {
                                    found_closing_brace = true;
                                    // Add content up to the closing brace
                                    scanned_content
                                        .push_str(&lines[current_line][..closing_brace_pos]);
                                    log::info!(
                                        "Found closing brace at line {}, scanned content: '{}'",
                                        current_line,
                                        scanned_content
                                    );
                                } else {
                                    // Add the entire line and continue to next
                                    scanned_content.push_str(lines[current_line]);
                                    scanned_content.push('\n');
                                    current_line += 1;
                                }
                            }

                            log::info!("Complete scanned content: '{}'", scanned_content);

                            // Check for keywords like "Self", "as", etc. in the scanned content
                            let common_keywords = ["Self", "as"];
                            for keyword in common_keywords.iter() {
                                if let Some(keyword_start) = scanned_content.find(keyword) {
                                    // Calculate the actual position in the original line context
                                    let keyword_start_pos = brace_pos + 1 + keyword_start;
                                    let keyword_end_pos = keyword_start_pos + keyword.len();

                                    if char_pos >= keyword_start_pos && char_pos <= keyword_end_pos
                                    {
                                        log::info!("Hovering on keyword: {}", keyword);

                                        // Special handling for Self keyword
                                        if *keyword == "Self" {
                                            target_stct_or_fn = "Self".to_string();
                                            found_target_stct_or_fn = true;
                                            // For Self keyword, show MODULE info (not item info)
                                            ref_module = full_module_name.clone();
                                            full_module_name_for_display = full_module_name.clone();
                                            capture_items_loc = use_decl.loc.clone();
                                            is_module_hover = true;
                                            log::info!("Found Self keyword, will show MODULE info");
                                        } else {
                                            target_stct_or_fn = keyword.to_string();
                                            found_target_stct_or_fn = true;
                                            // For other keywords, use full module path
                                            ref_module = full_module_name.clone();
                                            full_module_name_for_display = full_module_name.clone();
                                            capture_items_loc = use_decl.loc.clone();
                                            is_module_hover = false;
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Case 2: Direct item import without braces like "use aptos_framework::fungible_asset::Metadata"
                    // Check if we're hovering on the module part vs the item part
                    let char_pos = self.col as usize;
                    log::info!(
                        "No braces found, checking direct item import. Hover at char position: {}",
                        char_pos
                    );

                    // Check each module level to see which one we're hovering on
                    let module_parts: Vec<&str> = full_module_name.split("::").collect();
                    log::info!("Module parts: {:?}", module_parts);

                    // Safety check: make sure we have module parts to work with
                    if module_parts.is_empty() {
                        log::warn!("No module parts found, using fallback");
                        full_module_name_for_display = full_module_name.clone();
                        ref_module = full_module_name.clone();
                        target_stct_or_fn = full_module_name.clone();
                        found_target_stct_or_fn = true;
                        is_module_hover = true;
                        capture_items_loc = use_decl.loc.clone();
                        break;
                    }

                    // Find all :: positions in the line
                    let colon_positions: Vec<usize> = line_content
                        .char_indices()
                        .filter_map(|(i, c)| if c == ':' { Some(i) } else { None })
                        .collect();
                    log::info!("Colon positions: {:?}", colon_positions);

                    // Check if we're hovering on any module level
                    let mut found_module_level = false;
                    for (i, colon_pos) in colon_positions.iter().enumerate() {
                        // Safety check: make sure we have enough module parts
                        if i >= module_parts.len() {
                            log::warn!(
                                "Index {} out of bounds for module_parts (len: {})",
                                i,
                                module_parts.len()
                            );
                            continue;
                        }

                        if char_pos <= *colon_pos {
                            // Hovering on this module level
                            // Safety check: make sure we can create the slice [..=i]
                            let module_name = if i < module_parts.len() {
                                module_parts[..=i].join("::")
                            } else {
                                log::warn!("Cannot create slice [..={}], using fallback", i);
                                full_module_name.clone()
                            };
                            log::info!(
                                "Hovering on module level: {} (at position {})",
                                module_name,
                                colon_pos
                            );

                            full_module_name_for_display = module_name.clone();
                            // Safety check: make sure we can access module_parts[i]
                            let short_module_name = if i < module_parts.len() {
                                module_parts[i].to_string()
                            } else {
                                log::warn!("Index {} out of bounds, using fallback", i);
                                full_module_name.clone()
                            };
                            ref_module = short_module_name.clone();
                            target_stct_or_fn = short_module_name.clone();
                            found_target_stct_or_fn = true;
                            is_module_hover = true;
                            capture_items_loc = use_decl.loc.clone();
                            log::info!(
                                "Found use declaration on module level - module: {}, item: {}",
                                module_name,
                                target_stct_or_fn
                            );
                            found_module_level = true;
                            break;
                        }
                    }

                    if !found_module_level {
                        // Hovering after all ::, so it's on the final item
                        log::info!("Hovering on final item in direct item import");
                    }
                }

                for (member_loc, name, _) in use_decl.members.clone().into_iter() {
                    log::trace!("member_loc = {:?} ---", env.get_location(&member_loc));
                    log::info!(
                        "Checking member: {} at {:?}",
                        name.display(spool),
                        member_loc
                    );
                    if self.check_move_model_loc_contains_mouse_pos(env, &member_loc) {
                        log::trace!("find use symbol = {}", name.display(spool));
                        target_stct_or_fn = name.display(spool).to_string();
                        found_target_stct_or_fn = true;
                        let full_module_name = use_decl.module_name.display_full(env).to_string();
                        full_module_name_for_display = full_module_name.clone();
                        // Extract just the module name part for easier matching
                        let short_module_name =
                            if let Some(last_part) = full_module_name.split("::").last() {
                                last_part.to_string()
                            } else {
                                full_module_name.clone()
                            };
                        ref_module = short_module_name.clone();
                        log::info!(
                            "Extracted use declaration - full module: {}, short module: {}, item: {}",
                            full_module_name,
                            short_module_name,
                            target_stct_or_fn
                        );
                        capture_items_loc = member_loc;
                        is_module_hover = false; // This is an item hover, not module hover
                        break;
                    }
                }
            } else {
                log::info!("Use decl has no members, checking if hover is on the module name");
                // If no members, check if we're hovering on the module name itself
                if self.check_move_model_loc_contains_mouse_pos(env, &use_decl.loc) {
                    // Additional check: make sure we're not hovering on a member that might be in the same location
                    // Only show module info if this is truly a module-only use declaration
                    log::info!(
                        "Found use declaration with no members - this is a module-only import"
                    );
                    let full_module_name = use_decl.module_name.display_full(env).to_string();
                    full_module_name_for_display = full_module_name.clone();
                    let short_module_name =
                        if let Some(last_part) = full_module_name.split("::").last() {
                            last_part.to_string()
                        } else {
                            full_module_name.clone()
                        };
                    ref_module = short_module_name.clone();
                    target_stct_or_fn = short_module_name.clone();
                    found_target_stct_or_fn = true;
                    is_module_hover = true;
                    capture_items_loc = use_decl.loc.clone();
                    log::info!(
                        "Found use declaration on module name - module: {}, item: {}",
                        full_module_name,
                        target_stct_or_fn
                    );
                    break;
                }
            }
            if found_target_stct_or_fn {
                log::info!(
                    "Found target in use declaration: {} (module: {}, item: {}, is_module_hover: {})",
                    target_stct_or_fn,
                    ref_module,
                    target_stct_or_fn,
                    is_module_hover
                );
                break;
            }
        }

        if !found_target_stct_or_fn {
            log::info!("<on hover> not found_target_stct_or_fn in use_decl");
            log::info!(
                "No use declaration matched for hover at line: {}, col: {}",
                self.line,
                self.col
            );
            // Fallback: try to find use declarations within a few lines
            log::info!(
                "Trying fallback approach - looking for use declarations near line {}",
                self.line
            );
            for use_decl in target_module.get_use_decls() {
                if let Some(use_pos) = env.get_location(&use_decl.loc) {
                    let decl_line = u32::from(use_pos.line);
                    let line_diff = if decl_line > self.line {
                        decl_line - self.line
                    } else {
                        self.line - decl_line
                    };
                    log::info!("Found use_decl at line {} (diff: {})", decl_line, line_diff);

                    if line_diff <= 2 {
                        // Within 2 lines
                        log::info!("Use decl is close enough, checking if it has members");
                        if !use_decl.members.is_empty() {
                            // Just show the first member as an example
                            let full_module_name =
                                use_decl.module_name.display_full(env).to_string();
                            let result = format!(
                                "```move\nmodule {} {{\n    // Module contents\n}}\n```",
                                full_module_name
                            );
                            self.result_candidates.push(result);
                            return;
                        }
                    }
                }
            }
            return;
        }

        if self.capture_items_span_push(&capture_items_loc.span()) {
            // Extract documentation comments for the use declaration
            let docs = self.extract_documentation_comments(env, &capture_items_loc);

            log::info!(
                "Hover type: {} (is_module_hover: {})",
                if is_module_hover { "MODULE" } else { "ITEM" },
                is_module_hover
            );

            let result = if is_module_hover {
                // If hovering on module name, show module information
                let module_info = format!(
                    "```move\nmodule {} {{\n    // Module contents\n}}\n```",
                    full_module_name_for_display
                );

                log::info!("Showing MODULE information: {}", module_info);

                if docs.is_empty() {
                    module_info
                } else {
                    format!("{}\n\n**Documentation:**\n{}", module_info, docs)
                }
            } else {
                // If hovering on item, show item details
                log::info!(
                    "Getting item details for '{}' from module '{}'",
                    target_stct_or_fn,
                    ref_module
                );
                let item_details = self.get_item_details(env, &ref_module, &target_stct_or_fn);
                log::info!("Item details: {}", item_details);

                log::info!(
                    "Showing ITEM details for {} from module {}",
                    target_stct_or_fn,
                    ref_module
                );

                if docs.is_empty() {
                    item_details
                } else {
                    format!("{}\n\n**Documentation:**\n{}", item_details, docs)
                }
            };

            log::info!("Final result: {}", result);

            // Check if we already have similar module information to avoid duplication
            let is_duplicate = if is_module_hover {
                self.result_candidates.iter().any(|existing| {
                    existing.contains(&format!("module {}", full_module_name_for_display))
                        || existing.contains("// Module contents")
                        || existing.contains("// Friend module contents")
                })
            } else {
                false
            };

            if !is_duplicate {
                self.result_candidates.push(result);
                log::info!("Added result to candidates");
            } else {
                log::info!(
                    "Skipped duplicate result for {}",
                    full_module_name_for_display
                );
            }
        }
    }

    fn process_friend_decl(&mut self, env: &GlobalEnv) {
        let target_module = env.get_module(self.target_module_id);

        // Check if we can access friend declarations through the module
        // Since move-model may not have explicit friend support, we'll try to infer from the source
        let file_source = env.get_file_source(target_module.get_loc().file_id());
        let file_index = line_index::LineIndex::new(file_source);

        if let Some(line_offset_start) = file_index.line(self.line) {
            if let Some(line_offset_end) = file_index.offset(line_index::LineCol {
                line: self.line,
                col: self.col,
            }) {
                // Use full source line to extract module name so hover on partial token still shows full name
                let lines: Vec<&str> = file_source.lines().collect();
                if (self.line as usize) < lines.len() {
                    let full_line_source = lines[self.line as usize];
                    log::info!(
                        "Processing line for friend declaration: '{}'",
                        full_line_source
                    );
                    if full_line_source.contains("friend") {
                        log::info!("Line contains 'friend' keyword, extracting module name");
                        if let Some(friend_module) =
                            self.extract_friend_module_name(full_line_source)
                        {
                            log::info!("Found friend declaration for module: {}", friend_module);

                            // Check if this is a module-only friend declaration or has specific items
                            let is_module_only_friend = !full_line_source.contains("::");
                            log::info!(
                                "Friend declaration type: {} (contains '::': {})",
                                if is_module_only_friend {
                                    "MODULE_ONLY"
                                } else {
                                    "WITH_ITEMS"
                                },
                                full_line_source.contains("::")
                            );

                            if self.capture_items_span_push(&codespan::Span::new(
                                u32::from(line_offset_start.start()),
                                u32::from(line_offset_end),
                            )) {
                                // Extract documentation comments for the friend declaration
                                let docs =
                                    self.extract_documentation_comments_for_line(env, self.line);

                                // Friend declarations are always about modules, so show module information
                                let module_info = format!(
                                    "```move\nmodule {} {{\n    // Friend module contents\n}}\n```",
                                    friend_module
                                );

                                log::info!("Showing FRIEND MODULE information: {}", module_info);

                                let result = if docs.is_empty() {
                                    module_info
                                } else {
                                    format!("{}\n\n**Documentation:**\n{}", module_info, docs)
                                };

                                // Check if we already have similar module information to avoid duplication
                                let is_duplicate = self.result_candidates.iter().any(|existing| {
                                    existing.contains(&format!("module {}", friend_module))
                                        || existing.contains("// Module contents")
                                        || existing.contains("// Friend module contents")
                                });

                                if !is_duplicate {
                                    self.result_candidates.push(result);
                                    log::info!("Added friend module info to candidates");
                                } else {
                                    log::info!(
                                        "Skipped duplicate module info for {}",
                                        friend_module
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn extract_friend_module_name(&self, line_source: &str) -> Option<String> {
        // Parse friend abc::bcd; format
        let trimmed = line_source.trim();
        log::debug!("Extracting friend module name from: '{}'", trimmed);
        if trimmed.starts_with("friend") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            log::debug!("Friend declaration parts: {:?}", parts);
            if parts.len() >= 2 {
                let module_part = parts[1].replace(";", "");
                log::debug!("Extracted friend module: {}", module_part);
                Some(module_part)
            } else {
                log::debug!("Friend declaration has insufficient parts");
                None
            }
        } else {
            log::debug!("Line does not start with 'friend'");
            None
        }
    }

    fn process_const(&mut self, env: &GlobalEnv) {
        let target_module = env.get_module(self.target_module_id);
        for const_env in target_module.get_named_constants() {
            let this_const_loc = const_env.get_loc();
            let (_, const_start_pos) = env.get_file_and_location(&this_const_loc).unwrap();
            let (_, const_end_pos) = env
                .get_file_and_location(&move_model::model::Loc::new(
                    this_const_loc.file_id(),
                    codespan::Span::new(this_const_loc.span().end(), this_const_loc.span().end()),
                ))
                .unwrap();
            if const_start_pos.line.0 <= self.line && self.line <= const_end_pos.line.0 {
                self.process_type(env, &const_env.get_loc(), &const_env.get_type());
            }
        }
    }

    fn process_func(&mut self, env: &GlobalEnv) {
        let mut found_target_fun = false;
        let mut target_fun_id = FunId::new(env.symbol_pool().make("name"));
        let target_module = env.get_module(self.target_module_id);
        for fun in target_module.get_functions() {
            let this_fun_loc = fun.get_loc();
            let (_, func_start_pos) = env.get_file_and_location(&this_fun_loc).unwrap();
            let (_, func_end_pos) = env
                .get_file_and_location(&move_model::model::Loc::new(
                    this_fun_loc.file_id(),
                    codespan::Span::new(this_fun_loc.span().end(), this_fun_loc.span().end()),
                ))
                .unwrap();
            if func_start_pos.line.0 < self.line && self.line < func_end_pos.line.0 {
                target_fun_id = fun.get_id();
                found_target_fun = true;
                break;
            }
        }
        if !found_target_fun {
            log::warn!("<on hover> not found_target_fun");
            return;
        }

        let target_module = env.get_module(self.target_module_id);
        let target_fun = target_module.get_function(target_fun_id);
        let target_fun_loc = target_fun.get_loc();
        self.target_function_id = Some(target_fun.get_id());
        log::debug!("process function: {}", target_fun.get_name_string());
        self.get_mouse_loc(env, &target_fun_loc);
        if let Some(exp) = target_fun.get_def().as_deref() {
            self.process_expr(env, exp);
        };
        self.target_function_id = None;
    }

    fn process_spec_func(&mut self, env: &GlobalEnv) {
        let mut found_target_fun = false;
        let mut target_fun_id = FunId::new(env.symbol_pool().make("name"));
        let target_module = env.get_module(self.target_module_id);
        let mut spec_fn_span_loc = target_module.get_loc();

        for spec_block_info in target_module.get_spec_block_infos() {
            if let SpecBlockTarget::Function(_, fun_id) = spec_block_info.target {
                let span_first_col = move_model::model::Loc::new(
                    spec_block_info.loc.file_id(),
                    codespan::Span::new(
                        spec_block_info.loc.span().start(),
                        spec_block_info.loc.span().start() + codespan::ByteOffset(1),
                    ),
                );
                let span_last_col = move_model::model::Loc::new(
                    spec_block_info.loc.file_id(),
                    codespan::Span::new(
                        spec_block_info.loc.span().end(),
                        spec_block_info.loc.span().end() + codespan::ByteOffset(1),
                    ),
                );

                if let Some(s_loc) = env.get_location(&span_first_col) {
                    if let Some(e_loc) = env.get_location(&span_last_col) {
                        if u32::from(s_loc.line) <= self.line && self.line <= u32::from(e_loc.line)
                        {
                            target_fun_id = fun_id;
                            found_target_fun = true;
                            spec_fn_span_loc = spec_block_info.loc.clone();
                            break;
                        }
                    }
                }
            }
        }

        if !found_target_fun {
            log::warn!("<on hover> not found_target_spec_fun");
            return;
        }

        let target_fn = target_module.get_function(target_fun_id);
        let target_fn_spec = target_fn.get_spec();
        log::debug!("target_fun's spec = {}", env.display(&*target_fn_spec));
        self.get_mouse_loc(env, &spec_fn_span_loc);
        for cond in target_fn_spec.conditions.clone() {
            for exp in cond.all_exps() {
                self.process_expr(env, exp);
            }
        }
    }

    fn process_struct(&mut self, env: &GlobalEnv) {
        let mut found_target_struct = false;
        let mut target_struct_id = StructId::new(env.symbol_pool().make("name"));
        let target_module = env.get_module(self.target_module_id);
        for struct_env in target_module.get_structs() {
            let struct_loc = struct_env.get_loc();
            let (_, struct_start_pos) = env.get_file_and_location(&struct_loc).unwrap();
            let (_, struct_end_pos) = env
                .get_file_and_location(&move_model::model::Loc::new(
                    struct_loc.file_id(),
                    codespan::Span::new(struct_loc.span().end(), struct_loc.span().end()),
                ))
                .unwrap();
            if struct_start_pos.line.0 < self.line && self.line < struct_end_pos.line.0 {
                target_struct_id = struct_env.get_id();
                found_target_struct = true;
                break;
            }
        }

        if !found_target_struct {
            log::warn!("<on hover> not found_target_struct");
            return;
        }

        let target_module = env.get_module(self.target_module_id);
        let target_struct = target_module.get_struct(target_struct_id);
        let target_struct_loc = target_struct.get_loc();
        log::debug!("process struct: {}", target_struct.get_full_name_str());
        self.get_mouse_loc(env, &target_struct_loc);

        for field_env in target_struct.get_fields() {
            let field_name = field_env.get_name();
            let field_name_str = field_name.display(env.symbol_pool());
            let struct_source = env.get_source(&target_struct_loc);
            if let Ok(struct_str) = struct_source {
                if let Some(index) = struct_str.find(field_name_str.to_string().as_str()) {
                    let field_len = field_name_str.to_string().len();
                    let field_start = target_struct_loc.span().start()
                        + codespan::ByteOffset((index + field_len).try_into().unwrap());
                    // Assuming a relatively large distance
                    let field_end = field_start + codespan::ByteOffset((128).try_into().unwrap());
                    let field_loc = move_model::model::Loc::new(
                        target_struct_loc.file_id(),
                        codespan::Span::new(field_start, field_end),
                    );
                    let field_source = env.get_source(&field_loc);
                    if let Ok(atomic_field_str) = field_source {
                        if let Some(index) = atomic_field_str.find("\n".to_string().as_str()) {
                            let atomic_field_end =
                                field_start + codespan::ByteOffset(index.try_into().unwrap());
                            let atomic_field_loc = move_model::model::Loc::new(
                                target_struct_loc.file_id(),
                                codespan::Span::new(field_start, atomic_field_end),
                            );

                            if atomic_field_loc.span().end() < self.mouse_span.end()
                                || atomic_field_loc.span().start() > self.mouse_span.end()
                            {
                                continue;
                            }
                            let field_type = field_env.get_type();
                            self.process_type(env, &atomic_field_loc, &field_type);
                        }
                    }
                }
            }
        }
    }

    fn process_spec_struct(&mut self, env: &GlobalEnv) {
        let mut found_target_spec_stct = false;
        let mut target_stct_id = StructId::new(env.symbol_pool().make("name"));
        let target_module = env.get_module(self.target_module_id);
        let mut spec_stct_span_loc = target_module.get_loc();

        for spec_block_info in target_module.get_spec_block_infos() {
            if let SpecBlockTarget::Struct(_, stct_id) = spec_block_info.target {
                let span_first_col = move_model::model::Loc::new(
                    spec_block_info.loc.file_id(),
                    codespan::Span::new(
                        spec_block_info.loc.span().start(),
                        spec_block_info.loc.span().start() + codespan::ByteOffset(1),
                    ),
                );
                let span_last_col = move_model::model::Loc::new(
                    spec_block_info.loc.file_id(),
                    codespan::Span::new(
                        spec_block_info.loc.span().end(),
                        spec_block_info.loc.span().end() + codespan::ByteOffset(1),
                    ),
                );

                if let Some(s_loc) = env.get_location(&span_first_col) {
                    if let Some(e_loc) = env.get_location(&span_last_col) {
                        if u32::from(s_loc.line) <= self.line && self.line <= u32::from(e_loc.line)
                        {
                            target_stct_id = stct_id;
                            found_target_spec_stct = true;
                            spec_stct_span_loc = spec_block_info.loc.clone();
                            break;
                        }
                    }
                }
            }
        }

        if !found_target_spec_stct {
            log::warn!("<on hover> not found_target_spec_stct");
            return;
        }

        let target_stct = target_module.get_struct(target_stct_id);
        let target_stct_spec = target_stct.get_spec();
        log::debug!("target_stct's spec = {}", env.display(&*target_stct_spec));
        self.get_mouse_loc(env, &spec_stct_span_loc);
        for cond in target_stct_spec.conditions.clone() {
            for exp in cond.all_exps() {
                self.process_expr(env, exp);
            }
        }
    }

    fn process_expr(&mut self, env: &GlobalEnv, exp: &move_model::ast::Exp) {
        exp.visit_post_order(&mut |e| {
            match e {
                Value(node_id, _v) => {
                    // Const variable
                    let value_loc = env.get_node_loc(*node_id);
                    if self.check_move_model_loc_contains_mouse_pos(env, &value_loc) {
                        let mut value_str = String::default();
                        if let Ok(capture_value_str) = env.get_source(&value_loc) {
                            value_str = capture_value_str.to_string();
                        }
                        for named_const in
                            env.get_module(self.target_module_id).get_named_constants()
                        {
                            let spool = env.symbol_pool();
                            let named_const_str = named_const.get_name().display(spool).to_string();
                            if value_str.contains(&named_const_str) {
                                if self.capture_items_span_push(&value_loc.span()) {
                                    self.result_candidates
                                        .push(env.display(&named_const.get_value()).to_string());
                                }
                            }
                        }
                    }
                    true
                }
                Call(..) => {
                    self.process_call(env, e);
                    true
                }
                LocalVar(node_id, localvar_symbol) => {
                    let localvar_loc = env.get_node_loc(*node_id);

                    if localvar_loc.span().start() > self.mouse_span.end()
                        || self.mouse_span.end() > localvar_loc.span().end()
                    {
                        return true;
                    }

                    // The variable names "__update_iter_flag" and "__upper_bound_value" seem to be special,
                    // causing hover errors in a "for (i 1..10)" loop. Handling this issue accordingly.
                    if localvar_symbol.display(env.symbol_pool()).to_string()
                        == "__update_iter_flag"
                        || localvar_symbol.display(env.symbol_pool()).to_string()
                            == "__upper_bound_value"
                    {
                        return true;
                    }

                    log::trace!(
                        "local val symbol: {}",
                        localvar_symbol.display(env.symbol_pool()).to_string()
                    );

                    if let Some(node_type) = env.get_node_type_opt(*node_id) {
                        self.process_type(env, &localvar_loc, &node_type);
                    }
                    true
                }
                Temporary(node_id, _) => {
                    let tmpvar_loc = env.get_node_loc(*node_id);
                    if tmpvar_loc.span().start() > self.mouse_span.end()
                        || self.mouse_span.end() > tmpvar_loc.span().end()
                    {
                        return true;
                    }

                    if let Some(node_type) = env.get_node_type_opt(*node_id) {
                        self.process_type(env, &tmpvar_loc, &node_type);
                    }
                    true
                }
                Block(_, pattern, _, _) => {
                    self.match_pattern(env, pattern);
                    true
                }
                Assign(_, pattern, _) => {
                    self.match_pattern(env, pattern);
                    true
                }
                _ => true,
            }
        });
    }

    fn match_pattern(&mut self, env: &GlobalEnv, pattern: &Pattern) {
        match pattern {
            Pattern::Struct(node_id, q_sid, _, vec_p) => {
                let this_loc = env.get_node_loc(*node_id);
                if this_loc.span().start() > self.mouse_span.end()
                    || self.mouse_span.end() > this_loc.span().end()
                {
                    return;
                }

                // handle struct val
                for p in vec_p.iter() {
                    self.match_pattern(env, p);
                }
                // handle struct field
                self.process_struct_field(env, &this_loc, &q_sid.module_id, &q_sid.id);
            }
            Pattern::Tuple(nid, vec_p) => {
                let this_loc = env.get_node_loc(*nid);
                if this_loc.span().start() > self.mouse_span.end()
                    || self.mouse_span.end() > this_loc.span().end()
                {
                    return;
                }

                for p in vec_p.iter() {
                    self.match_pattern(env, p);
                }
            }
            Pattern::Var(nid, _) => {
                let this_loc = env.get_node_loc(*nid);
                if this_loc.span().start() > self.mouse_span.end()
                    || self.mouse_span.end() > this_loc.span().end()
                {
                    return;
                }

                if let Some(node_type) = env.get_node_type_opt(*nid) {
                    self.process_type(env, &this_loc, &node_type);
                }
            }
            _ => {}
        }
    }

    fn process_struct_field(
        &mut self,
        env: &GlobalEnv,
        this_loc: &move_model::model::Loc,
        module_id: &ModuleId,
        struct_id: &StructId,
    ) {
        let pattern_struct_source = env.get_source(this_loc).unwrap();
        let tok_vec = crate::utils::lexer_for_buffer(pattern_struct_source);
        for (_, pair) in tok_vec.windows(2).enumerate() {
            // search field(identifier), such as "identifier: val"
            if pair[0].0 != Tok::Identifier || pair[1].0 != Tok::Colon {
                continue;
            }

            let field_loc = move_model::model::Loc::new(
                this_loc.file_id(),
                Span::new(
                    this_loc.span().start() + codespan::ByteOffset(pair[0].1.0 as i64),
                    this_loc.span().start() + codespan::ByteOffset(pair[0].1.1 as i64),
                ),
            );

            if field_loc.span().start() > self.mouse_span.end()
                || self.mouse_span.end() > field_loc.span().end()
            {
                continue;
            }

            // find right field loc
            let field_source = env.get_source(&field_loc).unwrap_or("").to_string();
            let module_env = env.get_module(*module_id);
            let struct_env = module_env.get_struct(*struct_id);

            for field_env in struct_env.get_fields() {
                let field_name = field_env.get_name().display(env.symbol_pool()).to_string();
                if field_name == field_source {
                    self.process_type(env, &field_loc, &field_env.get_type());
                }
            }
        }
    }

    fn process_call(&mut self, env: &GlobalEnv, expdata: &move_model::ast::ExpData) {
        if let Call(node_id, MoveFunction(mid, fid), _) = expdata {
            let this_call_loc = env.get_node_loc(*node_id);

            if this_call_loc.span().start() < self.mouse_span.end()
                && self.mouse_span.end() < this_call_loc.span().end()
            {
                let called_module = env.get_module(*mid);
                let called_fun = called_module.get_function(*fid);
                if self.capture_items_span_push(&this_call_loc.span()) {
                    self.result_candidates.push(called_fun.get_header_string());
                }
            }
        }

        if let Call(node_id, Select(mid, sid, fid), _) = expdata {
            let this_call_loc = env.get_node_loc(*node_id);
            if this_call_loc.span().start() > self.mouse_span.end()
                || self.mouse_span.end() > this_call_loc.span().end()
            {
                return;
            }

            let called_module = env.get_module(*mid);
            let called_struct = called_module.get_struct(*sid);
            let called_field = called_struct.get_field(*fid);
            let field_type = called_field.get_type();
            self.process_type(env, &this_call_loc, &field_type);
        }

        if let Call(node_id, SpecFunction(mid, fid, _), _) = expdata {
            let this_call_loc = env.get_node_loc(*node_id);
            if this_call_loc.span().start() < self.mouse_span.end()
                && self.mouse_span.end() < this_call_loc.span().end()
            {
                let called_module = env.get_module(*mid);
                let spec_fun = called_module.get_spec_fun(*fid);
                if self.capture_items_span_push(&this_call_loc.span()) {
                    self.result_candidates
                        .push(spec_fun.name.display(env.symbol_pool()).to_string());
                }

                let inst_vec = &env.get_node_instantiation(*node_id);
                for inst in inst_vec {
                    let mut generic_ty_loc = this_call_loc.clone();
                    let capture_call_source = env.get_source(&this_call_loc);
                    if let Ok(capture_call_source_str) = capture_call_source {
                        if let Some(index) = capture_call_source_str.find("<".to_string().as_str())
                        {
                            generic_ty_loc = move_model::model::Loc::new(
                                this_call_loc.file_id(),
                                codespan::Span::new(
                                    this_call_loc.span().start()
                                        + codespan::ByteOffset(index.try_into().unwrap()),
                                    this_call_loc.span().end(),
                                ),
                            );
                        }
                    }
                    self.process_type(env, &generic_ty_loc, inst);
                }
            }
        }

        if let Call(node_id, Pack(mid, sid, _), _) = expdata {
            let op_loc = env.get_node_loc(*node_id);
            if op_loc.span().start() > self.mouse_span.end()
                || self.mouse_span.end() > op_loc.span().end()
            {
                return;
            }
            if let Some(node_type) = env.get_node_type_opt(*node_id) {
                self.process_type(env, &op_loc, &node_type);
            }

            self.process_struct_field(env, &op_loc, mid, sid);
        }
    }

    fn process_type(
        &mut self,
        env: &GlobalEnv,
        capture_items_loc: &move_model::model::Loc,
        ty: &move_model::ty::Type,
    ) {
        use move_model::ty::TypeDisplayContext;
        let display_context = TypeDisplayContext::new(env);
        let type_display = ty.display(&display_context);
        if self.capture_items_span_push(&(*capture_items_loc).span()) {
            self.result_candidates.push(type_display.to_string());
        }
    }

    fn run_move_model_visitor_internal(&mut self, env: &GlobalEnv, move_file_path: &Path) {
        let candidate_modules =
            crate::utils::get_modules_by_fpath_in_all_modules(env, &PathBuf::from(move_file_path));
        if candidate_modules.is_empty() {
            log::debug!("<on hover>cannot get target module\n");
            return;
        }
        for module_env in candidate_modules.iter() {
            self.target_module_id = module_env.get_id();
            if let Some(s) = move_file_path.to_str() {
                if s.contains(".spec") {
                    self.process_spec_func(env);
                    self.process_spec_struct(env);
                } else {
                    self.process_use_decl(env);
                    self.process_friend_decl(env); // Added friend declaration processing
                    self.process_const(env);
                    self.process_func(env);
                    self.process_struct(env);
                }
            }
        }
    }

    fn capture_items_span_push(&mut self, span: &Span) -> bool {
        if self.capture_items_span.contains(span) {
            return false;
        }
        self.capture_items_span.push(*span);
        true
    }

    fn extract_documentation_comments(
        &self,
        env: &GlobalEnv,
        loc: &move_model::model::Loc,
    ) -> String {
        let mut docs = String::new();
        let file_source = env.get_file_source(loc.file_id());
        let lines: Vec<&str> = file_source.lines().collect();

        if let Some(pos) = env.get_location(loc) {
            let line_num = pos.line.0 as usize;

            // Look for documentation comments above the declaration
            let mut i = line_num;
            while i > 0 {
                i -= 1;
                let line = lines[i].trim();
                if line.starts_with("///") {
                    docs.insert_str(0, &format!("{}\n", line.trim_start_matches('/').trim()));
                } else if line.starts_with("//") && !line.starts_with("///") {
                    // Skip regular comments
                    break;
                } else if line.is_empty() {
                    continue;
                } else {
                    break;
                }
            }
        }

        docs.trim().to_string()
    }

    fn extract_documentation_comments_for_line(&self, env: &GlobalEnv, line: u32) -> String {
        let target_module = env.get_module(self.target_module_id);

        let file_source = env.get_file_source(target_module.get_loc().file_id());
        let lines: Vec<&str> = file_source.lines().collect();

        let mut docs = String::new();
        let line_num = line as usize;

        // Look for documentation comments above the friend declaration
        let mut i = line_num;
        while i > 0 {
            i -= 1;
            let line_content = lines[i].trim();
            if line_content.starts_with("///") {
                docs.insert_str(
                    0,
                    &format!("{}\n", line_content.trim_start_matches('/').trim()),
                );
            } else if line_content.starts_with("//") && !line_content.starts_with("///") {
                // Skip regular comments
                break;
            } else if line_content.is_empty() {
                continue;
            } else {
                break;
            }
        }

        docs.trim().to_string()
    }

    fn get_item_details(&self, env: &GlobalEnv, module_name: &str, item_name: &str) -> String {
        log::info!(
            "Looking for item '{}' in module '{}'",
            item_name,
            module_name
        );
        // Find the module that contains the imported item
        let mut checked_modules = Vec::new();
        for module in env.get_modules() {
            let module_full_name = module.get_full_name_str();
            checked_modules.push(module_full_name.clone());
            log::info!("Checking module: {}", module_full_name);
            // Try to match the module name more accurately
            if self.module_names_match(module_name, &module_full_name) {
                log::info!("Module name matches, searching for item '{}'", item_name);
                let struct_count = module.get_structs().count();
                log::debug!("Module has {} structs", struct_count);
                let struct_names: Vec<String> = module
                    .get_structs()
                    .map(|s| s.get_name().display(env.symbol_pool()).to_string())
                    .collect();
                log::debug!("Struct names in module: {:?}", struct_names);

                // Check for functions
                for func in module.get_functions() {
                    let func_name = func.get_name_str();
                    let func_full_name = func.get_full_name_str();
                    log::debug!(
                        "Checking function: {} (full: {})",
                        func_name,
                        func_full_name
                    );
                    if func_name == item_name
                        || func_full_name == item_name
                        || func_name.contains(item_name)
                        || func_full_name.contains(item_name)
                    {
                        log::debug!("Found function: {}", func_name);
                        let func_sig = self.format_function_signature(&func, env);
                        return format!("{}\n{}", module_full_name, func_sig);
                    }
                }

                // Check for structs
                for stct in module.get_structs() {
                    let name = stct.get_name();
                    let stct_name = name.display(env.symbol_pool());
                    let stct_full_name = stct.get_full_name_str();
                    log::debug!("Checking struct: {} (full: {})", stct_name, stct_full_name);
                    log::debug!("Item name to match: '{}'", item_name);
                    log::debug!(
                        "Struct name == item_name: {}",
                        stct_name.to_string() == item_name
                    );
                    log::debug!(
                        "Struct full_name == item_name: {}",
                        stct_full_name == item_name
                    );
                    log::debug!(
                        "Struct name contains item_name: {}",
                        stct_name.to_string().contains(item_name)
                    );
                    log::debug!(
                        "Struct full_name contains item_name: {}",
                        stct_full_name.contains(item_name)
                    );
                    if stct_name.to_string() == item_name
                        || stct_full_name == item_name
                        || stct_name.to_string().contains(item_name)
                        || stct_full_name.contains(item_name)
                    {
                        log::debug!("Found struct: {}", stct_name);
                        log::debug!("About to format struct definition");
                        log::debug!("Calling format_struct_definition directly");
                        let result = self.format_struct_definition(&stct, env);
                        log::debug!("format_struct_definition returned: {}", result);
                        return format!("{}\n{}", module_full_name, result);
                    }
                }

                // Check for constants
                for const_env in module.get_named_constants() {
                    let const_name = const_env.get_name().display(env.symbol_pool()).to_string();
                    let const_full_name = const_env.module_env.get_full_name_str();
                    log::debug!(
                        "Checking constant: {} (full: {})",
                        const_name,
                        const_full_name
                    );
                    if const_name == item_name
                        || const_full_name == item_name
                        || const_name.contains(item_name)
                        || const_full_name.contains(item_name)
                    {
                        log::debug!("Found constant: {}", const_name);
                        let const_def = self.format_constant_definition(&const_env, env);
                        return format!("{}\n{}", module_full_name, const_def);
                    }
                }

                // If it's a module itself, show the module path
                if module.get_name().display(env).to_string() == item_name {
                    log::debug!(
                        "Found module: {}",
                        module.get_name().display(env).to_string()
                    );
                    // Count the module contents
                    let function_count = module.get_functions().count();
                    let struct_count = module.get_structs().count();
                    let constant_count = module.get_named_constants().count();

                    // Format as a Move module declaration with content summary
                    return format!(
                        "```move \nmodule {} {{\n    // Functions: {}\n    // Structs: {}\n    // Constants: {}\n}}\n```",
                        module_full_name, function_count, struct_count, constant_count
                    );
                }
            }
        }

        log::warn!(
            "Item '{}' not found in any module matching '{}'",
            item_name,
            module_name
        );
        log::info!("Checked modules: {:?}", checked_modules);
        // Fallback if item not found
        format!("**Type:** Unknown item")
    }

    fn module_names_match(&self, use_module_name: &str, actual_module_name: &str) -> bool {
        log::info!(
            "Comparing use module '{}' with actual module '{}'",
            use_module_name,
            actual_module_name
        );

        // First try exact match
        if use_module_name == actual_module_name {
            log::info!("Exact match found");
            return true;
        }

        // Extract just the module name part from the full module path
        // e.g., "aave_pool" from "0x1::aave_pool"
        if let Some(last_part) = actual_module_name.split("::").last() {
            let matches = last_part == use_module_name;
            log::info!(
                "Last part '{}' matches '{}': {}",
                last_part,
                use_module_name,
                matches
            );
            if matches {
                return true;
            }
        }

        // Try matching the full path
        if actual_module_name.contains(use_module_name) {
            log::info!("Full path contains use module name");
            return true;
        }

        // Try matching the last part of the use module name
        if let Some(last_part) = use_module_name.split("::").last() {
            if let Some(actual_last_part) = actual_module_name.split("::").last() {
                let matches = last_part == actual_last_part;
                log::info!(
                    "Last parts match: '{}' == '{}': {}",
                    last_part,
                    actual_last_part,
                    matches
                );
                if matches {
                    return true;
                }
            }
        }

        log::info!("No match found");
        false
    }

    fn format_function_signature(
        &self,
        func: &move_model::model::FunctionEnv,
        env: &GlobalEnv,
    ) -> String {
        let mut signature = format!(
            "```rust \npublic fun {}(",
            func.get_name().display(env.symbol_pool())
        );
        let context = TypeDisplayContext::new(env);
        // Add parameters
        let params: Vec<String> = func
            .get_parameters()
            .iter()
            .map(|param| {
                let param_type = param.1.display(&context);
                format!("{}: {}", param.0.display(env.symbol_pool()), param_type)
            })
            .collect();

        signature.push_str(&params.join(", "));
        signature.push_str(")");

        // Add return type if any
        let return_type = func.get_result_type();
        signature.push_str(&format!(" -> {}", return_type.display(&context)));

        signature.push_str("\n```");
        signature
    }

    fn format_struct_definition(
        &self,
        stct: &move_model::model::StructEnv,
        env: &GlobalEnv,
    ) -> String {
        let name = stct.get_name();
        let struct_name = name.display(env.symbol_pool());
        log::debug!("Formatting struct definition for: {}", struct_name);

        // Check if it's an enum or struct
        // For now, we'll treat all as structs since Move doesn't have traditional enums
        // but we can improve this later if needed
        let type_keyword = "struct";

        let mut definition = format!("```rust \n{} {} {{\n", type_keyword, struct_name);
        let context = TypeDisplayContext::new(env);

        // Add fields
        let field_count = stct.get_fields().count();
        log::debug!("Struct has {} fields", field_count);

        if field_count == 0 {
            definition.push_str("    // No fields\n");
        } else {
            for field in stct.get_fields() {
                let field_type = field.get_type();
                let name = field.get_name();
                let field_name = name.display(env.symbol_pool());
                let field_type_str = field_type.display(&context);
                log::debug!("Field: {}: {}", field_name, field_type_str);
                definition.push_str(&format!("    {}: {},\n", field_name, field_type_str));
            }
        }

        definition.push_str("}\n```");
        log::debug!("Final struct definition:\n{}", definition);
        definition
    }

    fn format_constant_definition(
        &self,
        const_env: &move_model::model::NamedConstantEnv,
        env: &GlobalEnv,
    ) -> String {
        let context = TypeDisplayContext::new(env);
        let binding = const_env.get_type();
        let const_type = binding.display(&context);
        format!(
            "```rust \nconst {}: {} = {:?}\n```",
            const_env.get_name().display(env.symbol_pool()),
            const_type,
            const_env.get_value()
        )
    }
}

impl ItemOrAccessHandler for Handler {
    fn visit_fun_or_spec_body(&self) -> bool {
        true
    }

    fn finished(&self) -> bool {
        false
    }

    fn handle_project_env(
        &mut self,
        _services: &dyn HandleItemService,
        env: &GlobalEnv,
        move_file_path: &Path,
        _: String,
    ) {
        self.run_move_model_visitor_internal(env, move_file_path);
    }
}

impl std::fmt::Display for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hover,file:{:?} line:{} col:{}",
            self.filepath, self.line, self.col
        )
    }
}

pub fn find_smallest_length_index(spans: &[codespan::Span]) -> Option<usize> {
    let mut smallest_length = i64::MAX;
    let mut smallest_index = None;

    for (index, span) in spans.iter().enumerate() {
        let length = span.end() - span.start();
        if length.0 < smallest_length {
            smallest_length = length.0;
            smallest_index = Some(index);
        }
    }

    smallest_index
}
