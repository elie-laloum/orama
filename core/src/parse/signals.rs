//! Analytics derived exclusively from normalized calls.

use std::collections::HashSet;

use super::model::*;

const OVERSIZED_CHARS: usize = 10_000;

pub fn intra_signals(thread: &[Turn], declared_tools: &[ToolDecl]) -> IntraSignals {
    let mut signals = IntraSignals::default();
    let declared: HashSet<&str> = declared_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    for (turn_index, turn) in thread.iter().enumerate() {
        for (block_index, block) in turn.blocks.iter().enumerate() {
            match block.kind {
                BlockKind::Text => signals.block_counts.text += 1,
                BlockKind::Thinking => signals.block_counts.thinking += 1,
                BlockKind::ToolUse => signals.block_counts.tool_use += 1,
                BlockKind::ToolResult => signals.block_counts.tool_result += 1,
                BlockKind::Image => signals.block_counts.image += 1,
                BlockKind::Other => signals.block_counts.other += 1,
            }
            if block.approx_size.chars >= OVERSIZED_CHARS {
                signals.oversized_blocks.push(BlockReference {
                    turn: turn_index,
                    block: block_index,
                    approx_size: block.approx_size.clone(),
                });
            }
            if matches!(block.kind, BlockKind::ToolUse) {
                let result = block
                    .tool_use_id
                    .as_deref()
                    .and_then(|id| find_tool_result(thread, id))
                    .cloned();
                let name = block
                    .tool_name
                    .clone()
                    .unwrap_or_else(|| "<unnamed>".to_string());
                if !declared.contains(name.as_str()) {
                    signals.undeclared_tool_calls.push(name.clone());
                }
                signals.tool_calls.push(ToolCall {
                    name,
                    input: block.input.clone(),
                    tool_use_id: block.tool_use_id.clone(),
                    result,
                });
            }
        }
    }
    signals
}

fn find_tool_result<'a>(thread: &'a [Turn], id: &str) -> Option<&'a Block> {
    thread.iter().flat_map(|turn| &turn.blocks).find(|block| {
        matches!(block.kind, BlockKind::ToolResult) && block.tool_use_id.as_deref() == Some(id)
    })
}
