//! The loss-aware, editable subset of Mermaid flowcharts.
//!
//! Parsing is deliberately strict: constructs that cannot be represented in the
//! visual editor return an error, so a visual edit never silently removes a
//! subgraph, style, directive, or another unsupported source construct.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Shape {
    #[default]
    Rectangle,
    Rounded,
    Stadium,
    Diamond,
    Circle,
}

impl Shape {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Rounded,
            2 => Self::Stadium,
            3 => Self::Diamond,
            4 => Self::Circle,
            _ => Self::Rectangle,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Rectangle => 0,
            Self::Rounded => 1,
            Self::Stadium => 2,
            Self::Diamond => 3,
            Self::Circle => 4,
        }
    }

    fn delimiters(self) -> (&'static str, &'static str) {
        match self {
            Self::Rectangle => ("[", "]"),
            Self::Rounded => ("(", ")"),
            Self::Stadium => ("([", "])"),
            Self::Diamond => ("{", "}"),
            Self::Circle => ("((", "))"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EdgeKind {
    #[default]
    Arrow,
    Line,
    Dotted,
    Thick,
}

impl EdgeKind {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Line,
            2 => Self::Dotted,
            3 => Self::Thick,
            _ => Self::Arrow,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Arrow => 0,
            Self::Line => 1,
            Self::Dotted => 2,
            Self::Thick => 3,
        }
    }

    fn connector(self) -> &'static str {
        match self {
            Self::Arrow => "-->",
            Self::Line => "---",
            Self::Dotted => "-.->",
            Self::Thick => "==>",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub shape: Shape,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Flow {
    pub direction: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Complete `%%` comment lines, retained when source is regenerated.
    pub comments: Vec<String>,
}

impl Default for Flow {
    fn default() -> Self {
        Self {
            direction: "TD".to_owned(),
            nodes: Vec::new(),
            edges: Vec::new(),
            comments: Vec::new(),
        }
    }
}

impl Flow {
    /// Parse a flowchart without discarding source constructs the editor cannot
    /// represent. Formatting is canonicalized by [`Self::to_mermaid`].
    pub fn parse(source: &str) -> Result<Self, String> {
        let (statements, comments) = statements(source)?;
        let Some((_, header)) = statements.first() else {
            return Err("Start with flowchart TD, TB, BT, LR, or RL.".to_owned());
        };
        let header: Vec<_> = header.split_whitespace().collect();
        if header.len() != 2 || !matches!(header[0], "flowchart" | "graph") {
            return Err(
                "Visual editing supports flowchart or graph diagrams with a direction.".to_owned(),
            );
        }
        if !matches!(header[1], "TD" | "TB" | "BT" | "LR" | "RL") {
            return Err("Use the flowchart direction TD, TB, BT, LR, or RL.".to_owned());
        }
        let mut flow = Self {
            direction: header[1].to_owned(),
            comments,
            ..Self::default()
        };
        for (line, statement) in statements.iter().skip(1) {
            flow.parse_statement(statement)
                .map_err(|message| format!("Line {line}: {message}"))?;
        }
        Ok(flow)
    }

    /// Serialize the editable document as canonical, safely quoted Mermaid.
    pub fn to_mermaid(&self) -> String {
        let mut output = format!("flowchart {}\n", self.direction);
        for comment in &self.comments {
            output.push_str("    ");
            output.push_str(comment);
            output.push('\n');
        }
        for node in &self.nodes {
            let (open, close) = node.shape.delimiters();
            output.push_str(&format!(
                "    {}{}\"{}\"{}\n",
                node.id,
                open,
                escape_label(&node.label),
                close
            ));
        }
        if !self.nodes.is_empty() && !self.edges.is_empty() {
            output.push('\n');
        }
        for edge in &self.edges {
            output.push_str(&format!("    {} {}", edge.from, edge.kind.connector()));
            if !edge.label.is_empty() {
                output.push_str(&format!("|\"{}\"|", escape_label(&edge.label)));
            }
            output.push_str(&format!(" {}\n", edge.to));
        }
        output
    }

    /// Append a node and return a fresh source identifier.
    pub fn add_node(&mut self, shape: Shape) -> String {
        let mut number = 1;
        let id = loop {
            let candidate = format!("N{number}");
            if self.node(&candidate).is_none() {
                break candidate;
            }
            number += 1;
        };
        self.nodes.push(Node {
            id: id.clone(),
            label: if shape == Shape::Diamond {
                "Decision".to_owned()
            } else {
                "New step".to_owned()
            },
            shape,
        });
        id
    }

    /// Remove a node and its incident edges in a single document operation.
    pub fn remove_node(&mut self, id: &str) {
        self.nodes.retain(|node| node.id != id);
        self.edges.retain(|edge| edge.from != id && edge.to != id);
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    fn parse_statement(&mut self, statement: &str) -> Result<(), String> {
        let mut cursor = Cursor::new(statement);
        let mut from = self.read_node(&mut cursor)?;
        loop {
            cursor.skip_space();
            if cursor.rest().is_empty() {
                return Ok(());
            }
            let (kind, label) = cursor.edge()?;
            let to = self.read_node(&mut cursor)?;
            self.edges.push(Edge {
                from,
                to: to.clone(),
                label,
                kind,
            });
            from = to;
        }
    }

    fn read_node(&mut self, cursor: &mut Cursor<'_>) -> Result<String, String> {
        cursor.skip_space();
        let id = cursor.identifier()?;
        cursor.skip_space();
        let declaration = cursor.node_label()?;
        if let Some((shape, label)) = declaration {
            if let Some(node) = self.node_mut(&id) {
                node.shape = shape;
                node.label = label;
            } else {
                self.nodes.push(Node {
                    id: id.clone(),
                    label,
                    shape,
                });
            }
        } else if self.node(&id).is_none() {
            self.nodes.push(Node {
                id: id.clone(),
                label: id.clone(),
                shape: Shape::Rectangle,
            });
        }
        Ok(id)
    }
}

type SourceStatements = (Vec<(usize, String)>, Vec<String>);

/// Split statements only outside labels and collect comments independently.
fn statements(source: &str) -> Result<SourceStatements, String> {
    let mut statements = Vec::new();
    let mut comments = Vec::new();
    let mut current = String::new();
    let mut closers = Vec::new();
    let mut quoted = false;
    let mut line = 1;
    let mut statement_line = 1;
    let mut chars = source.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(ch) = chars.next() {
        if !quoted && closers.is_empty() && ch == '%' && chars.peek() == Some(&'%') {
            let mut comment = String::from("%");
            while let Some(&next) = chars.peek() {
                if next == '\n' {
                    break;
                }
                comment.push(next);
                chars.next();
            }
            if comment
                .trim_start_matches('%')
                .trim_start()
                .starts_with('{')
            {
                return Err("Mermaid configuration directives are preserved in code mode; visual editing is unavailable.".to_owned());
            }
            comments.push(comment.trim_end().to_owned());
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        } else if !quoted {
            match ch {
                '[' => closers.push(']'),
                '(' => closers.push(')'),
                '{' => closers.push('}'),
                ']' | ')' | '}' if closers.pop() != Some(ch) => {
                    return Err(format!("Line {line}: unmatched '{ch}' in a node label."));
                }
                _ => {}
            }
        }
        if !quoted && closers.is_empty() && matches!(ch, '\n' | ';') {
            if !current.trim().is_empty() {
                statements.push((statement_line, current.trim().to_owned()));
            }
            current.clear();
            statement_line = line + usize::from(ch == '\n');
        } else {
            if current.trim().is_empty() && !ch.is_whitespace() {
                statement_line = line;
            }
            current.push(ch);
        }
        if ch == '\n' {
            line += 1;
        }
    }
    if quoted || !closers.is_empty() {
        return Err(format!(
            "Line {statement_line}: close the node label's quotes and brackets."
        ));
    }
    if !current.trim().is_empty() {
        statements.push((statement_line, current.trim().to_owned()));
    }
    Ok((statements, comments))
}

struct Cursor<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, offset: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.offset..]
    }

    fn skip_space(&mut self) {
        self.offset += self.rest().len() - self.rest().trim_start().len();
    }

    fn take(&mut self, prefix: &str) -> bool {
        if self.rest().starts_with(prefix) {
            self.offset += prefix.len();
            true
        } else {
            false
        }
    }

    fn identifier(&mut self) -> Result<String, String> {
        let start = self.offset;
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.offset += ch.len_utf8();
            } else if ch == '-'
                && self.offset > start
                && !self.rest().starts_with("--")
                && !self.rest().starts_with("-.")
            {
                self.offset += 1;
            } else {
                break;
            }
        }
        if self.offset == start {
            return Err("Expected a node ID. Visual editing supports letters, digits, underscores, and hyphens in IDs.".to_owned());
        }
        let id = &self.text[start..self.offset];
        if matches!(
            id,
            "subgraph"
                | "end"
                | "direction"
                | "style"
                | "classDef"
                | "class"
                | "linkStyle"
                | "click"
                | "accTitle"
                | "accDescr"
        ) {
            return Err(format!(
                "'{id}' is preserved in code mode; this construct is not supported by visual editing."
            ));
        }
        Ok(id.to_owned())
    }

    fn node_label(&mut self) -> Result<Option<(Shape, String)>, String> {
        let shape = if self.take("([") {
            Shape::Stadium
        } else if self.take("((") {
            Shape::Circle
        } else if self.take("[") {
            Shape::Rectangle
        } else if self.take("(") {
            Shape::Rounded
        } else if self.take("{") {
            Shape::Diamond
        } else {
            return Ok(None);
        };
        let (_, close) = shape.delimiters();
        let label = self.delimited_label(close)?;
        Ok(Some((shape, label)))
    }

    fn delimited_label(&mut self, close: &str) -> Result<String, String> {
        self.skip_space();
        let quoted = self.take("\"");
        let end = if quoted { "\"" } else { close };
        let Some(length) = self.rest().find(end) else {
            return Err("The label is missing its closing delimiter.".to_owned());
        };
        let raw = &self.rest()[..length];
        validate_label(raw, quoted)?;
        let label = decode_label(if quoted { raw } else { raw.trim() })?;
        self.offset += length;
        if quoted {
            self.offset += 1;
            self.skip_space();
        }
        if !self.take(close) {
            return Err("Unsupported label syntax after the quoted text.".to_owned());
        }
        Ok(label)
    }

    fn edge(&mut self) -> Result<(EdgeKind, String), String> {
        let mut inline_label = None;
        let kind = if self.take("-->") {
            EdgeKind::Arrow
        } else if self.take("---") {
            if self.rest().starts_with(['o', 'x']) {
                return Err("Circle and cross connections are preserved in code mode; visual editing supports plain lines and arrows.".to_owned());
            }
            EdgeKind::Line
        } else if self.take("-.->") {
            EdgeKind::Dotted
        } else if self.take("==>") {
            EdgeKind::Thick
        } else if self.take("--") {
            let (label, connector) = self.inline_edge_label(&["-->", "---"])?;
            inline_label = Some(label);
            if connector == "-->" {
                EdgeKind::Arrow
            } else {
                EdgeKind::Line
            }
        } else if self.take("-.") {
            inline_label = Some(self.inline_edge_label(&[".->"])?.0);
            EdgeKind::Dotted
        } else if self.take("==") {
            inline_label = Some(self.inline_edge_label(&["==>"])?.0);
            EdgeKind::Thick
        } else {
            return Err("Visual editing supports -->, ---, -.->, and ==> connections. Other source syntax stays available in code mode.".to_owned());
        };
        self.skip_space();
        let label = if let Some(label) = inline_label {
            if self.rest().starts_with('|') {
                return Err("An edge cannot have both inline and pipe-delimited labels.".to_owned());
            }
            label
        } else if self.take("|") {
            self.delimited_label("|")?
        } else {
            String::new()
        };
        Ok((kind, label))
    }

    fn inline_edge_label(&mut self, endings: &[&str]) -> Result<(String, String), String> {
        let mut quoted = false;
        for (offset, ch) in self.rest().char_indices() {
            if ch == '"' {
                quoted = !quoted;
            }
            if !quoted {
                for ending in endings {
                    if self.rest()[offset..].starts_with(ending) {
                        let raw = self.rest()[..offset].trim();
                        if raw.is_empty() {
                            return Err("Unsupported connection syntax.".to_owned());
                        }
                        let is_quoted =
                            raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2;
                        let raw = if is_quoted {
                            &raw[1..raw.len() - 1]
                        } else {
                            raw
                        };
                        validate_label(raw, is_quoted)?;
                        let label = decode_label(raw)?;
                        self.offset += offset + ending.len();
                        return Ok((label, (*ending).to_owned()));
                    }
                }
            }
        }
        Err("The connection or its label is incomplete.".to_owned())
    }
}

fn validate_label(raw: &str, quoted: bool) -> Result<(), String> {
    if raw.contains(['<', '>', '`']) {
        return Err("HTML and Markdown labels are preserved in code mode; visual editing supports plain text labels.".to_owned());
    }
    if raw.contains('"')
        || (!quoted
            && (raw.contains(['[', ']', '(', ')', '{', '}', '|'])
                || raw.starts_with(['/', '\\'])
                || raw.ends_with(['/', '\\'])))
    {
        return Err("Quote label text containing punctuation; this node or label syntax is not supported by visual editing.".to_owned());
    }
    Ok(())
}

fn escape_label(label: &str) -> String {
    let mut output = String::new();
    for ch in label.chars() {
        // Mermaid uses #number; entities. Encode syntax characters, including
        // the entity marker itself, so user text cannot become Mermaid syntax.
        if matches!(
            ch,
            '#' | '&'
                | '"'
                | '<'
                | '>'
                | '|'
                | '['
                | ']'
                | '('
                | ')'
                | '{'
                | '}'
                | '`'
                | '\n'
                | '\r'
        ) {
            output.push_str(&format!("#{};", u32::from(ch)));
        } else {
            output.push(ch);
        }
    }
    output
}

fn decode_label(raw: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = raw;
    while let Some(ch) = rest.chars().next() {
        if matches!(ch, '#' | '&')
            && let Some(end) = rest.find(';')
        {
            let entity = &rest[ch.len_utf8()..end];
            if !entity.is_empty()
                && (entity.chars().all(|c| c.is_ascii_alphanumeric())
                    || (ch == '&'
                        && entity.strip_prefix('#').is_some_and(|number| {
                            !number.is_empty() && number.chars().all(|c| c.is_ascii_alphanumeric())
                        })))
            {
                let decoded = match entity {
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "nbsp" => Some('\u{00a0}'),
                    "copy" => Some('©'),
                    "reg" => Some('®'),
                    _ => {
                        let number = if ch == '&' {
                            entity.strip_prefix('#')
                        } else {
                            Some(entity)
                        };
                        number.and_then(|number| {
                            if let Some(hex) = number
                                .strip_prefix('x')
                                .or_else(|| number.strip_prefix('X'))
                            {
                                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                            } else {
                                number.parse::<u32>().ok().and_then(char::from_u32)
                            }
                        })
                    }
                };
                let Some(decoded) = decoded else {
                    return Err(format!(
                        "The entity '{ch}{entity};' is preserved in code mode; visual editing cannot decode it."
                    ));
                };
                output.push(decoded);
                rest = &rest[end + 1..];
                continue;
            }
        }
        output.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shapes_labels_and_chain() {
        let flow = Flow::parse(
            "flowchart LR\nA[Start] --> B(Run) -->|yes| C([Finish])\nC -.-> D{Ready?}\nD ==> E((Done))\nE --- A",
        ).expect("supported flowchart");
        assert_eq!(flow.direction, "LR");
        assert_eq!(flow.nodes.len(), 5);
        assert_eq!(flow.edges.len(), 5);
        assert_eq!(flow.node("C").expect("C").shape, Shape::Stadium);
        assert_eq!(flow.node("D").expect("D").shape, Shape::Diamond);
        assert_eq!(flow.node("E").expect("E").shape, Shape::Circle);
        assert_eq!(flow.edges[1].label, "yes");
        assert_eq!(flow.edges[3].kind, EdgeKind::Thick);
        assert_eq!(
            Flow::parse(&flow.to_mermaid()).expect("generated source"),
            flow
        );
    }

    #[test]
    fn updates_referenced_nodes_and_preserves_comments() {
        let flow = Flow::parse(
            "%% A useful note\ngraph TD; A-->B %% An edge note\nB[The final label]\nA --> B[The latest label]",
        ).expect("valid graph");
        assert_eq!(flow.nodes.len(), 2);
        assert_eq!(flow.node("B").expect("B").label, "The latest label");
        assert_eq!(flow.comments, ["%% A useful note", "%% An edge note"]);
        assert_eq!(
            Flow::parse(&flow.to_mermaid()).expect("generated source"),
            flow
        );
    }

    #[test]
    fn supports_all_directions_and_inline_edge_labels() {
        for direction in ["TD", "TB", "BT", "LR", "RL"] {
            let flow = Flow::parse(&format!(
                "flowchart {direction}\nA -- next step --> B -. maybe .-> C == fast ==> D -- relation --- A"
            )).expect("valid labeled edges");
            assert_eq!(flow.direction, direction);
            assert_eq!(
                flow.edges
                    .iter()
                    .map(|edge| edge.label.as_str())
                    .collect::<Vec<_>>(),
                ["next step", "maybe", "fast", "relation"]
            );
            assert_eq!(
                Flow::parse(&flow.to_mermaid()).expect("generated source"),
                flow
            );
        }
    }

    #[test]
    fn safely_round_trips_user_text() {
        let mut flow = Flow::default();
        let a = flow.add_node(Shape::Rectangle);
        let b = flow.add_node(Shape::Circle);
        let label = "Quote \"; [box] (round) {shape} | #quot; & <tag> `code`\n日本語";
        flow.node_mut(&a).expect("created node").label = label.to_owned();
        flow.edges.push(Edge {
            from: a,
            to: b,
            label: label.to_owned(),
            kind: EdgeKind::Arrow,
        });
        assert_eq!(
            Flow::parse(&flow.to_mermaid()).expect("escaped source"),
            flow
        );
        assert_eq!(
            Flow::parse("flowchart TD\nA[\"#quot;hello#quot; &amp; #9829;\"]")
                .expect("entities")
                .nodes[0]
                .label,
            "\"hello\" & ♥"
        );
    }

    #[test]
    fn delimiters_inside_quoted_labels_are_text() {
        let flow =
            Flow::parse("flowchart TD; A[\"semi;colon [brackets] %% text\"] -->|\"a | b\"| B")
                .expect("quoted punctuation");
        assert_eq!(flow.nodes[0].label, "semi;colon [brackets] %% text");
        assert_eq!(flow.edges[0].label, "a | b");
        assert!(flow.comments.is_empty());
    }

    #[test]
    fn serialized_edge_entities_are_quoted_to_protect_semicolons() {
        let flow =
            Flow::parse("flowchart TD\nA -->|\"#34;yes#34;\"| B").expect("quoted edge entities");
        let source = flow.to_mermaid();
        assert!(source.contains("-->|\"#34;yes#34;\"|"));
        assert_eq!(Flow::parse(&source).expect("generated source"), flow);
        assert_eq!(flow.edges[0].label, "\"yes\"");
        assert_eq!(flow.nodes.len(), 2);
    }

    #[test]
    fn entity_decoding_preserves_literal_markers() {
        let flow = Flow::parse("flowchart TD\nA[\"##34; &#34; &#x2665;\"]")
            .expect("literal marker followed by entities");
        assert_eq!(flow.nodes[0].label, "#\" \" ♥");
        assert_eq!(
            Flow::parse(&flow.to_mermaid()).expect("generated source"),
            flow
        );
    }

    #[test]
    fn rejects_unsupported_source_instead_of_losing_it() {
        for source in [
            "flowchart TD\nsubgraph group\nA-->B\nend",
            "flowchart TD\nA-->B\nstyle A fill:red",
            "flowchart TD\nA:::class-->B",
            "flowchart TD\nA & B --> C",
            "flowchart TD\nA<-->B",
            "flowchart TD\nA---->B",
            "flowchart TD\nA-.-B",
            "flowchart TD\nA---oB",
            "flowchart TD\nA---xB",
            "flowchart TD\nA===B",
            "flowchart TD\nA[(Database)]",
            "flowchart TD\nA[/Parallelogram/]",
            "flowchart TD\nA[\"<b>Rich text</b>\"]",
            "flowchart TD\nA[\"`Markdown`\"]",
            "%%{init: {}}%%\nflowchart TD\nA-->B",
            "flowchart TD\nA-->B\nclick A \"https://example.com\"",
            "sequenceDiagram\nA->>B:Hello",
            "flowchart TD\nA[\"Unknown &entity;\"]",
            "flowchart TD\nA[\"Not an entity &123;\"]",
        ] {
            assert!(Flow::parse(source).is_err(), "must reject: {source}");
        }
    }

    #[test]
    fn rejects_incomplete_input_with_no_panics() {
        for source in [
            "",
            "flowchart",
            "flowchart UP",
            "flowchart TD\nA[",
            "flowchart TD\nA[\"x]",
            "flowchart TD\nA-->",
            "flowchart TD\nA -->|x B",
            "flowchart TD\nA]",
        ] {
            assert!(Flow::parse(source).is_err(), "must reject: {source}");
        }
    }

    #[test]
    fn node_removal_removes_incident_edges_and_ids_remain_unique() {
        let mut flow = Flow::parse("flowchart TD\nN1-->N2-->N3\nN3-->N1").expect("valid flow");
        assert_eq!(flow.add_node(Shape::Rounded), "N4");
        flow.remove_node("N2");
        assert!(flow.node("N2").is_none());
        assert_eq!(flow.edges.len(), 1);
        assert_eq!(flow.edges[0].from, "N3");
        assert_eq!(flow.add_node(Shape::Diamond), "N2");
        assert_eq!(
            Flow::parse(&flow.to_mermaid()).expect("generated source"),
            flow
        );
    }
}
