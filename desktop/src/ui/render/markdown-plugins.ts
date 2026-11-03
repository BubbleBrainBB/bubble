import remarkCjkFriendly from 'remark-cjk-friendly';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';

/**
 * The chat's remark pipeline (shared with tests). remark-cjk-friendly lets
 * `**` / `*` sit next to CJK punctuation (`**“重点”**之后`), which plain
 * CommonMark leaves as literal asterisks.
 */
export const MARKDOWN_REMARK_PLUGINS = [remarkGfm, remarkCjkFriendly, remarkMath];
