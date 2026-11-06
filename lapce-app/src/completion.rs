use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    str::FromStr,
    sync::Arc,
};

use floem::{
    peniko::kurbo::Rect,
    reactive::{ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    views::editor::{id::EditorId, text::Document},
};
use lapce_core::{
    buffer::rope_text::RopeText, movement::Movement, rope_text_pos::RopeTextPosition,
};
use lapce_rpc::{plugin::PluginId, proxy::ProxyRpcHandler};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    InsertTextFormat, Position,
};
use nucleo::Utf32Str;

use crate::{config::LapceConfig, doc::Doc, editor::EditorData, snippet::Snippet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompletionStatus {
    Inactive,
    Started,
    Done,
}

#[derive(Clone, PartialEq)]
pub struct ScoredCompletionItem {
    pub item: CompletionItem,
    pub plugin_id: PluginId,
    pub score: u32,
    pub label_score: u32,
    pub indices: Vec<usize>,
}

#[derive(Clone)]
pub struct CompletionData {
    pub status: CompletionStatus,
    /// The current request id. This is used to discard old requests.
    pub request_id: usize,
    /// An input id that is used for keeping track of whether the input has changed.
    pub input_id: usize,
    // TODO: A `PathBuf` has the issue that the proxy may not have the same format.
    // TODO(minor): It might be nice to not require a path. LSPs cannot operate on scratch buffers
    // as of now, but they might be allowed in the future.
    pub path: PathBuf,
    /// The offset that the completion is/was started at. Used for positioning the completion elem
    pub offset: usize,
    /// The active completion index in the list of filtered items
    pub active: RwSignal<usize>,
    /// The current input that the user has typed which is being sent for consideration by the LSP
    pub input: String,
    /// `(Input, CompletionItems)`
    pub input_items: im::HashMap<String, im::Vector<ScoredCompletionItem>>,
    /// The filtered items that are being displayed to the user
    pub filtered_items: im::Vector<ScoredCompletionItem>,
    /// The size of the completion element.  
    /// This is used for positioning the element.  
    /// As well, it is needed for some movement commands like page up/down that need to know the
    /// height to compute how far to move.
    pub layout_rect: Rect,
    /// The editor id that was most recently used to trigger a completion.
    pub latest_editor_id: Option<EditorId>,
    /// Matcher for filtering the completion items
    matcher: RwSignal<nucleo::Matcher>,
    config: ReadSignal<Arc<LapceConfig>>,
    /// The open documents, for word based suggestions (set once the main split exists)
    docs: Option<RwSignal<im::HashMap<PathBuf, Rc<Doc>>>>,
    /// The words of each open document with the revision they were counted at, so that a new
    /// completion only counts the documents that changed since the last one
    word_cache: Rc<RefCell<HashMap<PathBuf, (u64, Rc<HashMap<String, usize>>)>>>,
    /// The words of the open documents counted when the completion with this request id started.
    /// Typing only changes the word being typed, so the keystrokes of one completion reuse them
    /// instead of counting the document again for every new input
    session_words: Rc<RefCell<Option<(usize, Rc<HashMap<String, usize>>)>>>,
}

/// The provider id of word based suggestions. They come from the open documents, not from a plugin,
/// so they take an id no plugin gets (plugin ids count up from 0).
pub const WORD_COMPLETION_PLUGIN_ID: PluginId = PluginId(u64::MAX);

/// The most words to suggest, so that a huge document does not make every keystroke slow
const MAX_WORD_COMPLETION_ITEMS: usize = 5000;

impl CompletionData {
    pub fn new(cx: Scope, config: ReadSignal<Arc<LapceConfig>>) -> Self {
        let active = cx.create_rw_signal(0);
        Self {
            status: CompletionStatus::Inactive,
            request_id: 0,
            input_id: 0,
            path: PathBuf::new(),
            offset: 0,
            active,
            input: "".to_string(),
            input_items: im::HashMap::new(),
            filtered_items: im::Vector::new(),
            layout_rect: Rect::ZERO,
            matcher: cx
                .create_rw_signal(nucleo::Matcher::new(nucleo::Config::DEFAULT)),
            latest_editor_id: None,
            config,
            docs: None,
            word_cache: Rc::new(RefCell::new(HashMap::new())),
            session_words: Rc::new(RefCell::new(None)),
        }
    }

    /// Lets word based suggestions read the open documents
    pub fn set_docs(&mut self, docs: RwSignal<im::HashMap<PathBuf, Rc<Doc>>>) {
        self.docs = Some(docs);
    }

    /// Handle the response to a completion request.
    pub fn receive(
        &mut self,
        request_id: usize,
        input: &str,
        resp: &CompletionResponse,
        plugin_id: PluginId,
    ) {
        // If we've been canceled or the request id is old, ignore the response.
        if self.status == CompletionStatus::Inactive || self.request_id != request_id
        {
            return;
        }

        let items = match resp {
            CompletionResponse::Array(items) => items,
            // TODO: Possibly handle the 'is_incomplete' field on List.
            CompletionResponse::List(list) => &list.items,
        };
        let items: im::Vector<ScoredCompletionItem> = items
            .iter()
            .map(|i| ScoredCompletionItem {
                item: i.to_owned(),
                plugin_id,
                score: 0,
                label_score: 0,
                indices: Vec::new(),
            })
            .collect();
        // Several providers answer the same request (every plugin, and the words of the open
        // documents), so add to what has arrived instead of replacing it. A word that a plugin
        // also suggests is dropped, since the plugin's item knows more about it. A plugin's label
        // may decorate the name (rust-analyzer shows `foo(…)`), so the text the item filters and
        // inserts by counts as the name too.
        let labels: HashSet<&str> = items
            .iter()
            .flat_map(|i| {
                [
                    Some(i.item.label.as_str()),
                    i.item.filter_text.as_deref(),
                    i.item.insert_text.as_deref(),
                ]
            })
            .flatten()
            .collect();
        let mut merged: im::Vector<ScoredCompletionItem> = self
            .input_items
            .get(input)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| {
                i.plugin_id != WORD_COMPLETION_PLUGIN_ID
                    || !labels.contains(i.item.label.as_str())
            })
            .collect();
        merged.append(items);
        self.input_items.insert(input.to_string(), merged);
        self.filter_items();
    }

    /// Request for completion items wit the current request id.
    pub fn request(
        &mut self,
        editor_id: EditorId,
        proxy_rpc: &ProxyRpcHandler,
        path: PathBuf,
        input: String,
        position: Position,
    ) {
        self.latest_editor_id = Some(editor_id);
        let words = self.word_items(&path);
        self.input_items.insert(input.clone(), words);
        self.filter_items();
        proxy_rpc.completion(self.request_id, path, input, position);
    }

    /// Word based suggestions (`editor.word-based-suggestions`): the words of the open documents
    /// in the same language as `path`, like VS Code's `editor.wordBasedSuggestions`. They complete
    /// names even when no language server runs for the language.
    ///
    /// The word being typed is left out, unless it also appears somewhere else. Only the words that
    /// can match what is typed are kept, and the most frequent come first when there are too many.
    fn word_items(&self, path: &PathBuf) -> im::Vector<ScoredCompletionItem> {
        let config = self.config.get_untracked();
        let Some(docs) = self.docs.filter(|_| config.editor.word_based_suggestions)
        else {
            return im::Vector::new();
        };
        let counts = self.session_word_counts(path, docs);
        let mut words: Vec<(String, usize)> = counts
            .iter()
            .filter(|(word, count)| **count > 0 && may_match(word, &self.input))
            .map(|(word, count)| (word.clone(), *count))
            .collect();
        if words.len() > MAX_WORD_COMPLETION_ITEMS {
            words.sort_by(|(a, a_count), (b, b_count)| {
                b_count.cmp(a_count).then_with(|| a.cmp(b))
            });
            words.truncate(MAX_WORD_COMPLETION_ITEMS);
        }
        words
            .into_iter()
            .map(|(word, _)| ScoredCompletionItem {
                item: CompletionItem {
                    label: word,
                    kind: Some(CompletionItemKind::TEXT),
                    ..Default::default()
                },
                plugin_id: WORD_COMPLETION_PLUGIN_ID,
                score: 0,
                label_score: 0,
                indices: Vec::new(),
            })
            .collect()
    }

    /// The words of the open documents in the same language as `path` with how often they appear,
    /// counted once per completion (request id). The word being typed when the completion started
    /// is left out once, so it is suggested only when it also appears somewhere else.
    fn session_word_counts(
        &self,
        path: &PathBuf,
        docs: RwSignal<im::HashMap<PathBuf, Rc<Doc>>>,
    ) -> Rc<HashMap<String, usize>> {
        if let Some((request_id, counts)) = &*self.session_words.borrow() {
            if *request_id == self.request_id {
                return counts.clone();
            }
        }
        let docs = docs.get_untracked();
        let language = docs
            .get(path)
            .map(|doc| doc.syntax.with_untracked(|syntax| syntax.language));
        let mut cache = self.word_cache.borrow_mut();
        cache.retain(|path, _| docs.contains_key(path));
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (doc_path, doc) in docs.iter() {
            if doc.syntax.with_untracked(|syntax| Some(syntax.language)) != language
            {
                continue;
            }
            let rev = doc.buffer.with_untracked(|buffer| buffer.rev());
            let words = match cache.get(doc_path) {
                Some((counted_rev, words)) if *counted_rev == rev => words.clone(),
                _ => {
                    let text =
                        doc.buffer.with_untracked(|buffer| buffer.to_string());
                    let mut words: HashMap<String, usize> = HashMap::new();
                    for word in words_of(&text) {
                        *words.entry(word.to_string()).or_default() += 1;
                    }
                    let words = Rc::new(words);
                    cache.insert(doc_path.clone(), (rev, words.clone()));
                    words
                }
            };
            for (word, count) in words.iter() {
                *counts.entry(word.clone()).or_default() += count;
            }
        }
        if let Some(count) = counts.get_mut(&self.input) {
            *count -= 1;
        }
        let counts = Rc::new(counts);
        *self.session_words.borrow_mut() = Some((self.request_id, counts.clone()));
        counts
    }

    /// Close the completion, clearing all the data.
    pub fn cancel(&mut self) {
        if self.status == CompletionStatus::Inactive {
            return;
        }
        self.status = CompletionStatus::Inactive;
        self.input_id = 0;
        self.latest_editor_id = None;
        self.active.set(0);
        self.input.clear();
        self.input_items.clear();
        self.filtered_items.clear();
    }

    pub fn update_input(&mut self, input: String) {
        if self.status == CompletionStatus::Inactive {
            return;
        }
        self.input = input;
        // TODO: If the user types a letter that continues the current active item, we should
        // try keeping that item active. Possibly give this a setting.
        // ex: `p` has `print!` and `println!` has options. If you select the second, then type
        // `r` then it should stay on `println!` even as the overall filtering of the list changes.
        self.active.set(0);
        self.filter_items();
    }

    fn all_items(&self) -> im::Vector<ScoredCompletionItem> {
        self.input_items
            .get(&self.input)
            .cloned()
            .filter(|items| !items.is_empty())
            .unwrap_or_else(move || {
                self.input_items.get("").cloned().unwrap_or_default()
            })
    }

    pub fn filter_items(&mut self) {
        self.input_id += 1;
        if self.input.is_empty() {
            self.filtered_items = self.all_items();
            return;
        }

        // Filter the items by the fuzzy matching with the input text.
        let mut items: im::Vector<ScoredCompletionItem> = self
            .matcher
            .try_update(|matcher| {
                let pattern = nucleo::pattern::Pattern::parse(
                    &self.input,
                    nucleo::pattern::CaseMatching::Ignore,
                    nucleo::pattern::Normalization::Smart,
                );
                self.all_items()
                    .iter()
                    .filter_map(|i| {
                        let filter_text =
                            i.item.filter_text.as_ref().unwrap_or(&i.item.label);
                        let shift = i
                            .item
                            .label
                            .match_indices(filter_text)
                            .next()
                            .map(|(shift, _)| shift)
                            .unwrap_or(0);
                        let mut indices = Vec::new();
                        let mut filter_text_buf = Vec::new();
                        let filter_text =
                            Utf32Str::new(filter_text, &mut filter_text_buf);
                        if let Some(score) =
                            pattern.indices(filter_text, matcher, &mut indices)
                        {
                            if shift > 0 {
                                for idx in indices.iter_mut() {
                                    *idx += shift as u32;
                                }
                            }
                            let mut item = i.clone();
                            item.score = score;
                            item.label_score = score;
                            item.indices =
                                indices.into_iter().map(|i| i as usize).collect();

                            let mut label_buf = Vec::new();
                            let label_text =
                                Utf32Str::new(&i.item.label, &mut label_buf);
                            if let Some(score) = pattern.score(label_text, matcher) {
                                item.label_score = score;
                            }
                            Some(item)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap();
        // Sort all the items by their score, then their label score, then their length.
        items.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.label_score.cmp(&a.label_score))
                .then_with(|| a.item.label.len().cmp(&b.item.label.len()))
        });
        self.filtered_items = items;
    }

    /// Move down in the list of items.
    pub fn next(&mut self) {
        let active = self.active.get_untracked();
        let new =
            Movement::Down.update_index(active, self.filtered_items.len(), 1, true);
        self.active.set(new);
    }

    /// Move up in the list of items.
    pub fn previous(&mut self) {
        let active = self.active.get_untracked();
        let new =
            Movement::Up.update_index(active, self.filtered_items.len(), 1, true);
        self.active.set(new);
    }

    /// The amount of items that can be displayed in the current layout.
    fn display_count(&self) -> usize {
        let config = self.config.get_untracked();
        ((self.layout_rect.size().height / config.editor.line_height() as f64)
            .floor() as usize)
            .saturating_sub(1)
    }

    /// Move to the next page of items.
    pub fn next_page(&mut self) {
        let count = self.display_count();
        let active = self.active.get_untracked();
        let new = Movement::Down.update_index(
            active,
            self.filtered_items.len(),
            count,
            false,
        );
        self.active.set(new);
    }

    /// Move to the previous page of items.
    pub fn previous_page(&mut self) {
        let count = self.display_count();
        let active = self.active.get_untracked();
        let new = Movement::Up.update_index(
            active,
            self.filtered_items.len(),
            count,
            false,
        );
        self.active.set(new);
    }

    /// The currently selected/active item.
    pub fn current_item(&self) -> Option<&ScoredCompletionItem> {
        self.filtered_items.get(self.active.get_untracked())
    }

    /// Update the completion lens of the document with the active completion item.  
    pub fn update_document_completion(
        &self,
        editor_data: &EditorData,
        cursor_offset: usize,
    ) {
        let doc = editor_data.doc();

        if !doc.content.with_untracked(|content| content.is_file()) {
            return;
        }

        let config = self.config.get_untracked();

        if !config.editor.enable_completion_lens {
            doc.clear_completion_lens();
            return;
        }

        let completion_lens = completion_lens_text(
            doc.rope_text(),
            cursor_offset,
            self,
            doc.completion_lens().as_deref(),
        );
        match completion_lens {
            Some(Some(lens)) => {
                let offset = self.offset + self.input.len();
                // TODO: will need to be adjusted to use visual line.
                //   Could just store the offset in doc.
                let (line, col) = editor_data.editor.offset_to_line_col(offset);

                doc.set_completion_lens(lens, line, col);
            }
            // Unchanged
            Some(None) => {}
            None => {
                doc.clear_completion_lens();
            }
        }
    }
}

/// Get the text of the completion lens for the given completion item.  
/// Returns `None` if the completion lens should be hidden.
/// Returns `Some(None)` if the completion lens should be shown, but not changed.
/// Returns `Some(Some(text))` if the completion lens should be shown and changed to the given text.
fn completion_lens_text(
    rope_text: impl RopeText,
    cursor_offset: usize,
    completion: &CompletionData,
    current_completion: Option<&str>,
) -> Option<Option<String>> {
    let item = &completion.current_item()?.item;

    let item: Cow<str> = if let Some(edit) = &item.text_edit {
        // A text edit is used, because that is what will actually be inserted.

        let text_format = item
            .insert_text_format
            .unwrap_or(InsertTextFormat::PLAIN_TEXT);

        // We don't display insert and replace
        let CompletionTextEdit::Edit(edit) = edit else {
            return None;
        };
        // The completion offset can be different from the current cursor offset.
        let completion_offset = completion.offset;

        let start_offset = rope_text.prev_code_boundary(cursor_offset);
        let edit_start = rope_text.offset_of_position(&edit.range.start);

        // If the start of the edit isn't where the cursor currently is,
        // and it is not at the start of the completion, then we ignore it.
        // This captures most cases that we want, even if it skips over some
        // displayable edits.
        if start_offset != edit_start && completion_offset != edit_start {
            return None;
        }

        match text_format {
            InsertTextFormat::PLAIN_TEXT => {
                // This is not entirely correct because it assumes that the position is
                // `{start,end}_offset` when it may not necessarily be.
                Cow::Borrowed(&edit.new_text)
            }
            InsertTextFormat::SNIPPET => {
                // Parse the snippet. Bail if it's invalid.
                let snippet = Snippet::from_str(&edit.new_text).ok()?;

                let text = snippet.text();

                Cow::Owned(text)
            }
            _ => {
                // We don't know how to support this text format.
                return None;
            }
        }
    } else {
        // There's no specific text edit, so we just use the label.
        Cow::Borrowed(&item.label)
    };
    // We strip the prefix of the current input from the label.
    // So that, for example, `p` with a completion of `println` only sets the lens text to `rintln`.
    // If the text does not include a prefix in the expected position, then we do not display it.
    let item = item.as_ref().strip_prefix(&completion.input)?;

    // Get only the first line of text, because Lapce does not currently support
    // multi-line phantom text.
    let item = item.lines().next().unwrap_or(item);

    if Some(item) == current_completion {
        // If the item is the same as the current completion, then we don't display it.
        Some(None)
    } else {
        Some(Some(item.to_string()))
    }
}

/// The words of `text`: runs of letters, digits and `_` that do not start with a digit,
/// at least 2 characters long (a single letter is faster to type than to pick)
fn words_of(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| {
            word.chars().count() >= 2
                && !word.starts_with(|c: char| c.is_ascii_digit())
        })
}

/// Whether `word` contains the characters of `input` in order, ignoring case. The fuzzy filter
/// (`filter_items`) drops every other word anyway, so they are not worth keeping under the cap.
fn may_match(word: &str, input: &str) -> bool {
    let mut word = word.chars().flat_map(char::to_lowercase);
    input
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| word.any(|c| c == wanted))
}

#[cfg(test)]
mod word_tests {
    use super::{may_match, words_of};

    #[test]
    fn may_match_keeps_words_containing_the_input_in_order() {
        assert!(may_match("ContentView", "cview"));
        assert!(may_match("ContentView", ""));
        assert!(!may_match("ContentView", "vc"));
        assert!(!may_match("body", "bodyx"));
    }

    #[test]
    fn words_of_splits_on_symbols_and_skips_numbers_and_single_letters() {
        let words: Vec<&str> =
            words_of("struct ContentView: View { let x = 42; var body_1: some }")
                .collect();
        assert_eq!(
            words,
            vec![
                "struct",
                "ContentView",
                "View",
                "let",
                "var",
                "body_1",
                "some"
            ]
        );
    }

    #[test]
    fn words_of_keeps_non_ascii_identifiers() {
        let words: Vec<&str> = words_of("let 名前 = \"にんじゃ\"").collect();
        assert_eq!(words, vec!["let", "名前", "にんじゃ"]);
    }
}
