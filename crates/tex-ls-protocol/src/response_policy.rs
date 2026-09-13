//! Immutable, host-independent conversion of semantic responses to negotiated wire forms.
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct ResponsePolicy {
    capabilities: Value,
}

impl Default for ResponsePolicy {
    fn default() -> Self {
        Self::new(&Value::Null)
    }
}

impl ResponsePolicy {
    pub fn new(initialize: &Value) -> Self {
        Self {
            capabilities: initialize
                .get("capabilities")
                .cloned()
                .unwrap_or(Value::Null),
        }
    }

    pub fn supports(&self, path: &str) -> bool {
        self.capabilities.pointer(path).and_then(Value::as_bool) == Some(true)
    }

    pub fn literal_actions(&self) -> bool {
        self.capabilities
            .pointer("/textDocument/codeAction/codeActionLiteralSupport")
            .is_some_and(Value::is_object)
    }

    pub fn pull_diagnostics(&self) -> bool {
        self.capabilities
            .pointer("/textDocument/diagnostic")
            .is_some_and(Value::is_object)
    }

    pub fn initialize_result(&self, mut capabilities: lsp_types::ServerCapabilities) -> Value {
        if self.supports("/textDocument/rangeFormatting/rangesSupport") {
            capabilities.document_range_formatting_provider = Some(
                serde_json::from_value(json!({"rangesSupport":true}))
                    .expect("range formatting options"),
            );
        }
        if !self.literal_actions() {
            capabilities.code_action_provider = None;
        }
        if let Some(operations) = capabilities
            .workspace
            .as_mut()
            .and_then(|workspace| workspace.file_operations.as_mut())
        {
            if !self.supports("/workspace/fileOperations/willRename") {
                operations.will_rename = None;
            }
            if !self.supports("/workspace/fileOperations/didRename") {
                operations.did_rename = None;
            }
        }
        if self
            .capabilities
            .pointer("/textDocument/semanticTokens")
            .is_some()
            && self
                .capabilities
                .pointer("/textDocument/semanticTokens/formats")
                .and_then(Value::as_array)
                .is_some_and(|v| v.iter().any(|x| x == "relative"))
        {
            let types = self.semantic_types();
            if !types.is_empty() {
                capabilities.semantic_tokens_provider = Some(serde_json::from_value(json!({
                    "legend":{"tokenTypes":types,"tokenModifiers":[]},
                    "full": self.capabilities.pointer("/textDocument/semanticTokens/requests/full").is_some_and(|v| v == true || v.is_object()),
                    "range": self.capabilities.pointer("/textDocument/semanticTokens/requests/range").is_some_and(|value| value == true || value.is_object())
                })).expect("semantic token options"));
            }
        }
        json!({"capabilities": capabilities,
            "serverInfo": {"name": "tex-ls", "version": env!("CARGO_PKG_VERSION")}})
    }

    fn semantic_types(&self) -> Vec<&'static str> {
        crate::semantic_tokens::TYPES
            .iter()
            .copied()
            .filter(|kind| {
                self.capabilities
                    .pointer("/textDocument/semanticTokens/tokenTypes")
                    .and_then(Value::as_array)
                    .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(kind)))
            })
            .collect()
    }

    fn markdown(&self, path: &str) -> bool {
        self.capabilities
            .pointer(path)
            .and_then(Value::as_array)
            .and_then(|formats| {
                formats
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|format| matches!(*format, "markdown" | "plaintext"))
            })
            == Some("markdown")
    }

    /// Adapt once, after computation and before transport. Providers retain their
    /// semantic result; neither host contains representation-specific providers.
    pub fn response(&self, method: &str, uri: Option<&Value>, result: &mut Value) {
        if result.is_null() {
            return;
        }
        match method {
            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/range" => {
                let types = self.semantic_types();
                if let Some(data) = result.get_mut("data").and_then(Value::as_array_mut) {
                    let mut output = Vec::new();
                    let (mut line, mut column, mut last_line, mut last_column) = (0, 0, 0, 0);
                    for token in data.as_chunks::<5>().0 {
                        let delta = token[0].as_u64().unwrap_or(0);
                        line += delta;
                        column =
                            if delta == 0 { column } else { 0 } + token[1].as_u64().unwrap_or(0);
                        let kind = token[3].as_u64().unwrap_or(u64::MAX) as usize;
                        if let Some(index) = crate::semantic_tokens::TYPES
                            .get(kind)
                            .and_then(|kind| types.iter().position(|x| x == kind))
                        {
                            output.extend([
                                json!(line - last_line),
                                json!(if line == last_line {
                                    column - last_column
                                } else {
                                    column
                                }),
                                token[2].clone(),
                                json!(index),
                                json!(0),
                            ]);
                            (last_line, last_column) = (line, column);
                        }
                    }
                    *data = output;
                }
            }
            "textDocument/completion" => {
                let items = if result.is_array() {
                    result.as_array_mut()
                } else {
                    result.get_mut("items").and_then(Value::as_array_mut)
                };
                if let Some(items) = items {
                    for item in items {
                        self.completion(item);
                    }
                }
                if result.is_object() {
                    self.completion_defaults(result);
                }
            }
            "completionItem/resolve" => self.completion(result),
            "textDocument/foldingRange" => {
                if let Some(ranges) = result.as_array_mut() {
                    for range in ranges.iter_mut() {
                        if let Some(object) = range.as_object_mut() {
                            if !self
                                .supports("/textDocument/foldingRange/foldingRange/collapsedText")
                            {
                                object.remove("collapsedText");
                            }
                            if let Some(kinds) = self
                                .capabilities
                                .pointer("/textDocument/foldingRange/foldingRangeKind/valueSet")
                                .and_then(Value::as_array)
                                && object.get("kind").is_some_and(|kind| !kinds.contains(kind))
                            {
                                object.remove("kind");
                            }
                        }
                        if self.supports("/textDocument/foldingRange/lineFoldingOnly")
                            && let Some(object) = range.as_object_mut()
                        {
                            object.remove("startCharacter");
                            if object.remove("endCharacter").is_some()
                                && let Some(line) = object.get("endLine").and_then(Value::as_u64)
                            {
                                object.insert("endLine".into(), json!(line.saturating_sub(1)));
                            }
                        }
                    }
                    ranges.retain(|range| range["startLine"].as_u64() < range["endLine"].as_u64());
                    if let Some(limit) = self
                        .capabilities
                        .pointer("/textDocument/foldingRange/rangeLimit")
                        .and_then(Value::as_u64)
                    {
                        ranges.truncate(limit as usize);
                    }
                }
            }
            "textDocument/definition" => {
                if !self.supports("/textDocument/definition/linkSupport")
                    && let Some(links) = result.as_array_mut()
                {
                    for link in links {
                        if link.get("targetUri").is_some() {
                            *link = json!({"uri":link["targetUri"], "range":link["targetSelectionRange"]});
                        }
                    }
                }
            }
            "textDocument/hover" => {
                if let Some(contents) = result.get_mut("contents") {
                    markup(contents, self.markdown("/textDocument/hover/contentFormat"));
                }
            }
            "textDocument/signatureHelp" => {
                if result.get("activeParameter").is_some_and(Value::is_null)
                    && !self.supports(
                        "/textDocument/signatureHelp/signatureInformation/noActiveParameterSupport",
                    )
                {
                    *result = Value::Null;
                    return;
                }
                if let Some(signatures) = result.get_mut("signatures").and_then(Value::as_array_mut)
                {
                    for signature in signatures {
                        if !self.supports("/textDocument/signatureHelp/signatureInformation/activeParameterSupport") {
                            signature.as_object_mut().expect("signature object").remove("activeParameter");
                        }
                        if !self.supports("/textDocument/signatureHelp/signatureInformation/parameterInformation/labelOffsetSupport") {
                            let label: Vec<_> = signature["label"].as_str().unwrap_or_default().encode_utf16().collect();
                            if let Some(parameters) = signature.get_mut("parameters").and_then(Value::as_array_mut) {
                                for parameter in parameters {
                                    if let Some(offsets) = parameter.get("label").and_then(Value::as_array)
                                        && let [start, end] = offsets.as_slice()
                                        && let (Some(start), Some(end)) = (start.as_u64(), end.as_u64())
                                        && let Some(text) = label.get(start as usize..end as usize) {
                                        parameter["label"] = json!(String::from_utf16_lossy(text));
                                    }
                                }
                            }
                        }
                    }
                }

                self.documentation(
                    result,
                    self.markdown(
                        "/textDocument/signatureHelp/signatureInformation/documentationFormat",
                    ),
                );
            }
            "textDocument/documentSymbol" => {
                self.symbols(result, "/textDocument/documentSymbol/symbolKind/valueSet");
                if !self.supports("/textDocument/documentSymbol/hierarchicalDocumentSymbolSupport")
                    && let Some(items) = result.as_array_mut()
                {
                    let mut flat = Vec::new();
                    flatten_symbols(
                        std::mem::take(items),
                        uri.unwrap_or(&Value::Null),
                        None,
                        &mut flat,
                    );
                    *result = Value::Array(flat);
                }
            }
            "workspace/symbol" => self.symbols(result, "/workspace/symbol/symbolKind/valueSet"),
            "textDocument/codeAction" => {
                if !self.literal_actions() {
                    *result = json!([]);
                    return;
                }
                if let Some(actions) = result.as_array_mut() {
                    if !self.supports("/textDocument/codeAction/disabledSupport") {
                        actions.retain(|action| action.get("disabled").is_none());
                    }
                    if self
                        .capabilities
                        .pointer("/textDocument/linkedEditingRange")
                        .is_some()
                    {
                        actions.retain(|action| {
                            action.get("kind").and_then(Value::as_str)
                                != Some("refactor.rewrite.environment")
                        });
                    }
                    for action in actions {
                        if let Some(action) = action.as_object_mut() {
                            if !self.supports("/textDocument/codeAction/isPreferredSupport") {
                                action.remove("isPreferred");
                            }
                            if !self.supports("/textDocument/codeAction/disabledSupport") {
                                action.remove("disabled");
                            }
                            if !self.supports("/textDocument/codeAction/dataSupport") {
                                action.remove("data");
                            }
                            if let Some(diags) = action.get_mut("diagnostics") {
                                self.diagnostics(diags, self.pull_diagnostics());
                            }
                        }
                    }
                }
            }
            "textDocument/diagnostic" | "workspace/diagnostic" => self.diagnostics(result, true),
            "textDocument/publishDiagnostics" => {
                if !self.supports("/textDocument/publishDiagnostics/versionSupport")
                    && let Some(params) = result.as_object_mut()
                {
                    params.remove("version");
                }
                self.diagnostics(result, false);
            }
            _ => {}
        }
    }

    fn completion_defaults(&self, result: &mut Value) {
        let Some(supported) = self
            .capabilities
            .pointer("/textDocument/completion/completionList/itemDefaults")
            .and_then(Value::as_array)
        else {
            return;
        };
        let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) else {
            return;
        };
        if items.len() < 2 {
            return;
        }
        let mut defaults = serde_json::Map::new();
        for field in ["insertTextFormat", "insertTextMode"] {
            if supported.iter().any(|v| v == field)
                && let Some(value) = items[0].get(field).cloned()
                && items.iter().all(|item| item.get(field) == Some(&value))
            {
                defaults.insert(field.into(), value);
                for item in items.iter_mut() {
                    item.as_object_mut().expect("item").remove(field);
                }
            }
        }
        if supported.iter().any(|v| v == "editRange") {
            let edit_range = |item: &Value| -> Option<Value> {
                let edit = item.get("textEdit")?;
                edit.get("range").cloned().or_else(|| {
                    Some(json!({"insert":edit.get("insert")?, "replace":edit.get("replace")?}))
                })
            };
            if let Some(range) = edit_range(&items[0])
                && items
                    .iter()
                    .all(|item| edit_range(item) == Some(range.clone()))
            {
                defaults.insert("editRange".into(), range);
                for item in items.iter_mut() {
                    if let Some(edit) = item.as_object_mut().expect("item").remove("textEdit") {
                        item["textEditText"] = edit["newText"].clone();
                    }
                }
            }
        }
        if !defaults.is_empty() {
            result["itemDefaults"] = Value::Object(defaults);
        }
    }

    fn completion(&self, item: &mut Value) {
        let Some(item) = item.as_object_mut() else {
            return;
        };
        if !self.supports("/textDocument/completion/completionItem/preselectSupport") {
            item.remove("preselect");
        }
        if self.supports("/textDocument/completion/completionItem/insertReplaceSupport")
            && let Some(edit) = item.get_mut("textEdit").and_then(Value::as_object_mut)
            && edit
                .get("range")
                .is_some_and(|range| range["start"]["line"] == range["end"]["line"])
            && let Some(replace) = edit.remove("range")
        {
            // Equal ranges are a valid prefix relation. Both client acceptance
            // modes consume the whole key instead of retaining an obsolete suffix.
            edit.insert("insert".into(), replace.clone());
            edit.insert("replace".into(), replace);
        }
        if item.get("insertTextFormat") == Some(&json!(2))
            && !self.supports("/textDocument/completion/completionItem/snippetSupport")
        {
            // Structural snippets have a plain name as their semantic fallback.
            let plain = item.get("label").cloned().unwrap_or(json!(""));
            item.insert("insertText".into(), plain.clone());
            item.insert("insertTextFormat".into(), json!(1));
            if let Some(edit) = item.get_mut("textEdit").and_then(Value::as_object_mut) {
                edit.insert("newText".into(), plain);
            }
        }
        if !self.supports("/textDocument/completion/completionItem/preselectSupport") {
            item.remove("preselect");
        }
        if let Some(kind) = item.get_mut("kind") {
            self.kind(
                kind,
                "/textDocument/completion/completionItemKind/valueSet",
                18,
                1,
            );
        }
        if let Some(doc) = item.get_mut("documentation") {
            markup(
                doc,
                self.markdown("/textDocument/completion/completionItem/documentationFormat"),
            );
        }
    }

    fn kind(&self, kind: &mut Value, path: &str, legacy_max: u64, fallback: u64) {
        if let Some(allowed) = self.capabilities.pointer(path).and_then(Value::as_array) {
            if !allowed.contains(kind) {
                *kind = allowed.first().cloned().unwrap_or(json!(fallback));
            }
        } else if kind.as_u64().is_some_and(|kind| kind > legacy_max) {
            *kind = json!(fallback);
        }
    }

    fn symbols(&self, value: &mut Value, path: &str) {
        if let Some(items) = value.as_array_mut() {
            for item in items {
                if let Some(kind) = item.get_mut("kind") {
                    self.kind(kind, path, 18, 1);
                }
                if let Some(children) = item.get_mut("children") {
                    self.symbols(children, path);
                }
            }
        }
    }

    fn documentation(&self, value: &mut Value, markdown: bool) {
        // Signature and parameter documentation share the signature capability.
        fn walk(value: &mut Value, markdown: bool) {
            match value {
                Value::Object(object) => {
                    for (key, value) in object {
                        if key == "documentation" {
                            markup(value, markdown);
                        } else {
                            walk(value, markdown);
                        }
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        walk(item, markdown);
                    }
                }
                _ => {}
            }
        }
        walk(value, markdown);
    }

    fn diagnostics(&self, value: &mut Value, pull: bool) {
        let prefix = if pull {
            "/textDocument/diagnostic"
        } else {
            "/textDocument/publishDiagnostics"
        };
        match value {
            Value::Array(items) => {
                for item in items {
                    self.diagnostics(item, pull);
                }
            }
            Value::Object(object) => {
                if object.contains_key("message") && object.contains_key("range") {
                    // This capability guards diagnostic messages in every route,
                    // including push and diagnostics echoed by code actions.
                    if self.supports("/textDocument/diagnostic/markupMessageSupport")
                        && object.get("source").and_then(Value::as_str) == Some("tex-ls")
                        && object.get("code").and_then(Value::as_str).is_some()
                        && let Some(message) = object.get("message").and_then(Value::as_str)
                        && let Some(markdown) = diagnostic_markdown(message)
                    {
                        object.insert(
                            "message".into(),
                            json!({"kind":"markdown", "value":markdown}),
                        );
                    }
                    for (field, capability) in [
                        ("relatedInformation", "relatedInformation"),
                        ("codeDescription", "codeDescriptionSupport"),
                        ("data", "dataSupport"),
                    ] {
                        if !self.supports(&format!("{prefix}/{capability}")) {
                            object.remove(field);
                        }
                    }
                    if let Some(tags) = object.get_mut("tags").and_then(Value::as_array_mut) {
                        let allowed = self
                            .capabilities
                            .pointer(&format!("{prefix}/tagSupport/valueSet"))
                            .and_then(Value::as_array);
                        tags.retain(|tag| allowed.is_some_and(|allowed| allowed.contains(tag)));
                        if tags.is_empty() {
                            object.remove("tags");
                        }
                    }
                } else {
                    for child in object.values_mut() {
                        self.diagnostics(child, pull);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Lint messages use backticks to delimit source spellings. Escape all prose so
/// source-derived punctuation cannot introduce links or HTML. An unmatched
/// delimiter remains literal; clients without markup support get the original
/// string, without a lossy Markdown round trip.
fn diagnostic_markdown(message: &str) -> Option<String> {
    fn escape(out: &mut String, text: &str) {
        for ch in text.chars() {
            if ch.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(ch);
        }
    }
    let mut result = String::new();
    let mut rest = message;
    let mut has_code = false;
    while let Some(start) = rest.find('`') {
        let Some(end) = rest[start + 1..].find('`').map(|end| start + 1 + end) else {
            break;
        };
        let code = &rest[start + 1..end];
        if code.is_empty() || code.contains(['\r', '\n']) || code.trim() != code {
            escape(&mut result, &rest[..=end]);
        } else {
            escape(&mut result, &rest[..start]);
            result.push('`');
            result.push_str(code);
            result.push('`');
            has_code = true;
        }
        rest = &rest[end + 1..];
    }
    escape(&mut result, rest);
    has_code.then_some(result)
}

// Consume the hierarchical response so a large outline never retains two full
// copies of its ranges and names while adapting to a flat-symbol client.
fn flatten_symbols(
    items: Vec<Value>,
    uri: &Value,
    container: Option<&Value>,
    out: &mut Vec<Value>,
) {
    for mut item in items {
        let children = item.get_mut("children").map(Value::take);
        let name = item["name"].take();
        let parent = name.clone();
        let kind = item["kind"].take();
        let range = item["selectionRange"].take();
        drop(item);
        let mut location = serde_json::Map::new();
        location.insert("uri".into(), uri.clone());
        location.insert("range".into(), range);
        let mut symbol = serde_json::Map::new();
        symbol.insert("name".into(), name);
        symbol.insert("kind".into(), kind);
        symbol.insert("location".into(), Value::Object(location));
        if let Some(container) = container {
            symbol.insert("containerName".into(), container.clone());
        }
        out.push(Value::Object(symbol));
        if let Some(Value::Array(children)) = children {
            flatten_symbols(children, uri, Some(&parent), out);
        }
    }
}

fn markup(value: &mut Value, markdown: bool) {
    if !markdown && value.get("kind").and_then(Value::as_str) == Some("markdown") {
        let text = plain_text(value["value"].as_str().unwrap_or_default());
        *value = json!({"kind": "plaintext", "value": text});
    }
}

/// Render Markdown as text, preserving code, paragraph boundaries and link targets.
fn plain_text(markdown: &str) -> String {
    let mut text = String::new();
    let mut links = Vec::new();
    for event in Parser::new(markdown) {
        match event {
            Event::Text(value)
            | Event::Code(value)
            | Event::Html(value)
            | Event::InlineHtml(value) => text.push_str(&value),
            Event::SoftBreak | Event::HardBreak | Event::Rule => text.push('\n'),
            Event::Start(Tag::Link { dest_url, .. }) => links.push(dest_url),
            Event::End(TagEnd::Link) => {
                if let Some(url) = links.pop() {
                    text.push_str(&format!(" ({url})"));
                }
            }
            Event::End(
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Item,
            ) => text.push('\n'),
            _ => {}
        }
    }
    text.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_markup_is_negotiated_on_every_report_and_action_route() {
        let message = "unknown field `a_b` in <entry>; see [literal](https://example.org)";
        let diagnostic =
            json!({"range":{},"source":"tex-ls","code":"unknown-field","message":message});
        for (method, envelope, pointer) in [
            (
                "textDocument/diagnostic",
                json!({"kind":"full","items":[diagnostic]}),
                "/items/0/message",
            ),
            (
                "workspace/diagnostic",
                json!({"items":[{"items":[diagnostic]}]}),
                "/items/0/items/0/message",
            ),
            (
                "textDocument/publishDiagnostics",
                json!({"diagnostics":[diagnostic]}),
                "/diagnostics/0/message",
            ),
            (
                "textDocument/codeAction",
                json!([{"title":"Fix", "diagnostics":[diagnostic]}]),
                "/0/diagnostics/0/message",
            ),
        ] {
            for markup_support in [false, true] {
                let policy = ResponsePolicy::new(&json!({"capabilities":{"textDocument":{
                    "diagnostic":{"markupMessageSupport":markup_support},
                    "codeAction":{"codeActionLiteralSupport":{}}
                }}}));
                let mut result = envelope.clone();
                policy.response(method, None, &mut result);
                let rendered = result.pointer(pointer).unwrap();
                if markup_support {
                    assert_eq!(rendered["kind"], "markdown");
                    let markdown = rendered["value"].as_str().unwrap();
                    assert_eq!(plain_text(markdown), message.replace('`', ""));
                    assert!(Parser::new(markdown).all(|event| !matches!(
                        event,
                        Event::Start(Tag::Link { .. }) | Event::Html(_) | Event::InlineHtml(_)
                    )));
                } else {
                    assert_eq!(rendered, message);
                }
            }
        }
        assert!(diagnostic_markdown("plain message").is_none());
        assert!(diagnostic_markdown("unmatched `source").is_none());
        assert!(diagnostic_markdown("empty `` value").is_none());
    }

    #[test]
    fn minimal_client_gets_plain_insertions_and_rendered_text() {
        let policy = ResponsePolicy::default();
        let mut items = json!({"items":[{"label":"itemize","kind":25,"insertText":"itemize}\n\t$0\n\\end{itemize}","insertTextFormat":2,"preselect":true,"documentation":{"kind":"markdown","value":"**Bold** and `code` [Docs](https://example.org)"}}]});
        policy.response("textDocument/completion", None, &mut items);
        assert_eq!(items["items"][0]["insertText"], "itemize");
        assert_eq!(items["items"][0]["insertTextFormat"], 1);
        assert_eq!(items["items"][0]["kind"], 1);
        assert!(items["items"][0].get("preselect").is_none());
        assert_eq!(
            items["items"][0]["documentation"],
            json!({"kind":"plaintext","value":"Bold and code Docs (https://example.org)"})
        );
        assert_eq!(
            plain_text("```latex\n\\foo{a_b} % keep *literal*\n```"),
            "\\foo{a_b} % keep *literal*"
        );
    }

    #[test]
    fn rich_and_restricted_clients_get_their_selected_representations() {
        let policy = ResponsePolicy::new(&json!({"capabilities":{"textDocument":{
            "completion":{"completionItem":{"snippetSupport":true,"documentationFormat":["markdown"]},"completionItemKind":{"valueSet":[3]}},
            "hover":{"contentFormat":["plaintext","markdown"]}
        }}}));
        let mut item = json!({"label":"env","kind":25,"insertText":"env$0","insertTextFormat":2,"documentation":{"kind":"markdown","value":"**Text**"}});
        policy.response("completionItem/resolve", None, &mut item);
        assert_eq!(item["insertText"], "env$0");
        assert_eq!(item["kind"], 3);
        assert_eq!(item["documentation"]["kind"], "markdown");
        let mut hover = json!({"contents":{"kind":"markdown","value":"**Text**"}});
        policy.response("textDocument/hover", None, &mut hover);
        assert_eq!(
            hover["contents"],
            json!({"kind":"plaintext","value":"Text"})
        );
    }

    #[test]
    fn flat_symbols_keep_exact_selections_and_parent_names() {
        let range = json!({"start":{"line":0,"character":1},"end":{"line":0,"character":3}});
        let mut symbols = json!([{"name":"Parent","kind":2,"range":range,"selectionRange":range,"children":[{"name":"Child","kind":26,"range":range,"selectionRange":range}]}]);
        ResponsePolicy::default().response(
            "textDocument/documentSymbol",
            Some(&json!(fixture_uri!("/a.tex"))),
            &mut symbols,
        );
        assert_eq!(symbols.as_array().unwrap().len(), 2);
        assert_eq!(symbols[1]["containerName"], "Parent");
        assert_eq!(
            symbols[1]["location"],
            json!({"uri":fixture_uri!("/a.tex"),"range":range})
        );
        assert_eq!(symbols[1]["kind"], 1);
    }

    #[test]
    fn optional_actions_and_diagnostic_fields_are_negotiated() {
        let policy = ResponsePolicy::default();
        let mut actions = json!([{"title":"Fix","isPreferred":true}]);
        policy.response("textDocument/codeAction", None, &mut actions);
        assert_eq!(actions, json!([]));
        let mut diagnostic = json!({"items":[{"message":"Bad","range":{},"tags":[1,2],"relatedInformation":[],"codeDescription":{"href":"https://example.org"},"data":{}}]});
        policy.response("textDocument/diagnostic", None, &mut diagnostic);
        assert_eq!(diagnostic, json!({"items":[{"message":"Bad","range":{}}]}));
        let init = policy.initialize_result(crate::server_capabilities(
            crate::PositionEncoding::Utf16,
            false,
        ));
        assert!(init["capabilities"].get("codeActionProvider").is_none());
        assert_eq!(init["serverInfo"]["name"], "tex-ls");
    }
}
