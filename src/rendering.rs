use crate::cli::{OutputFormat, OutputStyle};
use indexmap::IndexMap;
use serde::Serialize;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::rc::Rc;
use tabled::settings::location::ByColumnName;
use tabled::settings::{Remove, Style};
use tabled::{Table, Tabled};

pub trait RenderToString {
    fn render(&self, str: &str);
}

pub struct PrintRender {}

impl PrintRender {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl RenderToString for PrintRender {
    fn render(&self, str: &str) {
        println!("{}", str.trim_end());
    }
}

pub struct Rendering {
    output: OutputFormat,
    output_long: bool,
    output_style: OutputStyle,
    sink: Box<dyn RenderToString>,
}

impl Rendering {
    pub fn new(
        output: OutputFormat,
        output_style: OutputStyle,
        output_long: bool,
        sink: Box<dyn RenderToString>,
    ) -> Self {
        Self {
            output,
            output_long,
            output_style,
            sink,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self.output, OutputFormat::Text)
    }

    pub fn is_flat(&self) -> bool {
        matches!(self.output, OutputFormat::Text | OutputFormat::Csv)
    }

    fn configure_table_no_headers(&self, table: &mut Table) {
        match self.output_style {
            OutputStyle::Blank => table.with(Style::blank()),
            OutputStyle::Markdown => table.with(Style::markdown().remove_horizontals()),
            OutputStyle::Rounded => table.with(Style::rounded().remove_horizontals()),
        };
    }
    fn configure_table(&self, table: &mut Table) {
        match self.output_style {
            OutputStyle::Blank => table.with(Style::blank()),
            OutputStyle::Markdown => table.with(Style::markdown()),
            OutputStyle::Rounded => table.with(Style::rounded()),
        };
    }

    /// Render a list of items in the selected output format.
    ///
    /// Flat formats (text table, csv) render one row per item as produced by `to_row`;
    /// nested formats (json, yaml) serialize the items themselves. `long_columns` are
    /// hidden from the table unless `long` or the global `--output-long` is set.
    pub fn render_list<T, R, F>(
        &self,
        items: &[T],
        to_row: F,
        long: &bool,
        long_columns: &[&str],
    ) -> anyhow::Result<()>
    where
        T: Serialize,
        R: Tabled + Serialize,
        F: Fn(&T) -> R,
    {
        if self.is_flat() {
            let rows: Vec<R> = items.iter().map(to_row).collect();
            self.render_flat_list(&rows, long, long_columns)
        } else {
            self.render_nested_list(items)
        }
    }

    /// Render a list whose items are already table rows (see [`Rendering::render_list`]).
    pub fn render_rows<T>(
        &self,
        items: &[T],
        long: &bool,
        long_columns: &[&str],
    ) -> anyhow::Result<()>
    where
        T: Tabled + Serialize,
    {
        if self.is_flat() {
            self.render_flat_list(items, long, long_columns)
        } else {
            self.render_nested_list(items)
        }
    }

    /// Render a single item: a key/value table (from `to_dict`) for flat formats,
    /// the serialized item itself for nested formats.
    pub fn render_single<T, F>(&self, item: &T, to_dict: F) -> anyhow::Result<()>
    where
        T: Serialize,
        F: Fn(&T) -> IndexMap<String, String>,
    {
        if self.is_flat() {
            self.render_flat_single(&to_dict(item))
        } else {
            self.render_nested_single(item)
        }
    }

    pub fn render_nested_list<T: Serialize>(&self, object: &[T]) -> anyhow::Result<()> {
        match self.output {
            OutputFormat::Text => anyhow::bail!("Nested not supported in text output"),
            OutputFormat::Csv => anyhow::bail!("Nested not supported in csv output"),
            OutputFormat::Json => {
                let output = serde_json::to_string_pretty(object)?;
                self.sink.render(&output);
            }
            OutputFormat::Yaml => {
                let output = serde_yaml_ng::to_string(object)?;
                self.sink.render(&output);
            }
        }
        Ok(())
    }

    pub fn render_flat_list<T: Tabled + Serialize>(
        &self,
        object: &[T],
        long: &bool,
        long_columns: &[&str],
    ) -> anyhow::Result<()> {
        match self.output {
            OutputFormat::Json => anyhow::bail!("Flat not supported in Json output"),
            OutputFormat::Yaml => anyhow::bail!("Flat not supported in Yaml output"),
            OutputFormat::Text => {
                let mut table = Table::new(object);
                self.configure_table(&mut table);
                if !self.output_long && !long {
                    for col in long_columns {
                        table.with(Remove::column(ByColumnName::new(*col)));
                    }
                }
                self.sink.render(table.to_string().as_str());
            }
            OutputFormat::Csv => {
                let mut wtr = csv::Writer::from_writer(vec![]);
                for record in object {
                    wtr.serialize(record)?;
                }
                wtr.flush()?;
                let result = String::from_utf8(wtr.into_inner()?)?;
                self.sink.render(&result);
            }
        }
        Ok(())
    }

    pub fn render_nested_single<T: Serialize>(&self, object: &T) -> anyhow::Result<()> {
        match self.output {
            OutputFormat::Text => anyhow::bail!("Nested not supported in text output"),
            OutputFormat::Csv => anyhow::bail!("Nested not supported in csv output"),
            OutputFormat::Json => {
                let output = serde_json::to_string_pretty(object)?;
                self.sink.render(&output);
            }
            OutputFormat::Yaml => {
                let output = serde_yaml_ng::to_string(object)?;
                self.sink.render(&output);
            }
        }
        Ok(())
    }

    pub fn render_flat_single(&self, object: &IndexMap<String, String>) -> anyhow::Result<()> {
        match self.output {
            OutputFormat::Json => anyhow::bail!("Flat not supported in Json output"),
            OutputFormat::Yaml => anyhow::bail!("Flat not supported in Yaml output"),
            OutputFormat::Text => {
                let mut table = Table::nohead(object);
                self.configure_table_no_headers(&mut table);
                self.sink.render(table.to_string().as_str());
            }
            OutputFormat::Csv => {
                let mut wtr = csv::Writer::from_writer(vec![]);
                for record in object {
                    wtr.serialize(record)?;
                }
                wtr.flush()?;
                let result = String::from_utf8(wtr.into_inner()?)?;
                self.sink.render(&result);
            }
        }
        Ok(())
    }

    pub(crate) fn print(&self, text: &str) {
        self.sink.render(text);
    }
}

#[cfg(test)]
pub struct StringRender {
    pub string: Rc<RefCell<String>>,
}
#[cfg(test)]
impl StringRender {
    pub fn new() -> Self {
        Self {
            string: Rc::new(RefCell::new(String::new())),
        }
    }
}
#[cfg(test)]
impl RenderToString for StringRender {
    fn render(&self, str: &str) {
        self.string.borrow_mut().push_str(str);
        self.string.borrow_mut().push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use tabled::Tabled;

    #[derive(Tabled, Serialize)]
    struct Row {
        id: String,
        name: String,
        description: String,
    }

    fn rows() -> Vec<Row> {
        vec![Row {
            id: "1".into(),
            name: "one".into(),
            description: "first".into(),
        }]
    }

    fn rendering(
        format: OutputFormat,
        style: OutputStyle,
        long: bool,
    ) -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let output = sink.string.clone();
        (Rendering::new(format, style, long, sink), output)
    }

    #[test]
    fn format_predicates() {
        assert!(
            rendering(OutputFormat::Text, OutputStyle::Rounded, false)
                .0
                .is_text()
        );
        assert!(
            rendering(OutputFormat::Text, OutputStyle::Rounded, false)
                .0
                .is_flat()
        );
        assert!(
            rendering(OutputFormat::Csv, OutputStyle::Rounded, false)
                .0
                .is_flat()
        );
        assert!(
            !rendering(OutputFormat::Csv, OutputStyle::Rounded, false)
                .0
                .is_text()
        );
        assert!(
            !rendering(OutputFormat::Json, OutputStyle::Rounded, false)
                .0
                .is_flat()
        );
        assert!(
            !rendering(OutputFormat::Yaml, OutputStyle::Rounded, false)
                .0
                .is_flat()
        );
    }

    #[test]
    fn flat_list_hides_long_columns_unless_requested() {
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Rounded, false);
        r.render_flat_list(&rows(), &false, &["description"])
            .unwrap();
        assert_eq!(
            out.borrow().as_str(),
            "╭────┬──────╮\n│ id │ name │\n├────┼──────┤\n│ 1  │ one  │\n╰────┴──────╯\n"
        );

        // Per-command --long
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Rounded, false);
        r.render_flat_list(&rows(), &true, &["description"])
            .unwrap();
        assert!(out.borrow().contains("description"));

        // Global --output-long
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Rounded, true);
        r.render_flat_list(&rows(), &false, &["description"])
            .unwrap();
        assert!(out.borrow().contains("description"));
    }

    #[test]
    fn flat_list_ignores_unknown_long_columns() {
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Rounded, false);
        r.render_flat_list(&rows(), &false, &["does_not_exist"])
            .unwrap();
        assert!(out.borrow().contains("description"));
    }

    #[test]
    fn flat_list_styles() {
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Markdown, true);
        r.render_flat_list(&rows(), &false, &[]).unwrap();
        assert_eq!(
            out.borrow().as_str(),
            "| id | name | description |\n|----|------|-------------|\n| 1  | one  | first       |\n"
        );

        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Blank, true);
        r.render_flat_list(&rows(), &false, &[]).unwrap();
        assert_eq!(
            out.borrow().as_str(),
            " id   name   description \n 1    one    first       \n"
        );
    }

    #[test]
    fn flat_list_csv_goes_through_the_sink_with_header() {
        let (r, out) = rendering(OutputFormat::Csv, OutputStyle::Rounded, false);
        r.render_flat_list(&rows(), &false, &["description"])
            .unwrap();
        assert_eq!(
            out.borrow().as_str(),
            "id,name,description\n1,one,first\n\n"
        );
    }

    #[test]
    fn flat_single_text_and_csv() {
        let mut map = IndexMap::new();
        map.insert("key".to_string(), "value".to_string());
        map.insert("other".to_string(), "x, y".to_string());

        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Markdown, false);
        r.render_flat_single(&map).unwrap();
        assert_eq!(
            out.borrow().as_str(),
            "| key   | value |\n| other | x, y  |\n"
        );

        let (r, out) = rendering(OutputFormat::Csv, OutputStyle::Rounded, false);
        r.render_flat_single(&map).unwrap();
        assert_eq!(out.borrow().as_str(), "key,value\nother,\"x, y\"\n\n");
    }

    #[test]
    fn nested_json_and_yaml() {
        let (r, out) = rendering(OutputFormat::Json, OutputStyle::Rounded, false);
        r.render_nested_list(&rows()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["name"], "one");

        let (r, out) = rendering(OutputFormat::Yaml, OutputStyle::Rounded, false);
        r.render_nested_single(&rows()[0]).unwrap();
        assert_eq!(
            out.borrow().as_str(),
            "id: '1'\nname: one\ndescription: first\n\n"
        );
    }

    #[test]
    fn unsupported_format_combinations_are_errors() {
        let (r, _) = rendering(OutputFormat::Text, OutputStyle::Rounded, false);
        assert!(r.render_nested_list(&rows()).is_err());
        assert!(r.render_nested_single(&rows()[0]).is_err());

        let (r, _) = rendering(OutputFormat::Json, OutputStyle::Rounded, false);
        assert!(r.render_flat_list(&rows(), &false, &[]).is_err());
        assert!(r.render_flat_single(&IndexMap::new()).is_err());
    }

    #[test]
    fn render_list_picks_rows_for_flat_and_items_for_nested() {
        #[derive(Serialize)]
        struct Item {
            id: u32,
            nested: Vec<u32>,
        }
        #[derive(Tabled, Serialize)]
        struct ItemRow {
            id: u32,
            count: usize,
        }
        let items = vec![Item {
            id: 7,
            nested: vec![1, 2, 3],
        }];
        let to_row = |item: &Item| ItemRow {
            id: item.id,
            count: item.nested.len(),
        };

        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Markdown, false);
        r.render_list(&items, to_row, &false, &["count"]).unwrap();
        assert_eq!(out.borrow().as_str(), "| id |\n|----|\n| 7  |\n");

        let (r, out) = rendering(OutputFormat::Json, OutputStyle::Markdown, false);
        r.render_list(&items, to_row, &false, &["count"]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["nested"][2], 3);
    }

    #[test]
    fn render_rows_uses_items_as_rows() {
        let (r, out) = rendering(OutputFormat::Csv, OutputStyle::Markdown, true);
        r.render_rows(&rows(), &false, &[]).unwrap();
        assert_eq!(
            out.borrow().as_str(),
            "id,name,description\n1,one,first\n\n"
        );
    }

    #[test]
    fn render_single_picks_dict_for_flat_and_item_for_nested() {
        let to_dict = |row: &Row| {
            let mut map = IndexMap::new();
            map.insert("id".to_string(), row.id.clone());
            map
        };
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Markdown, false);
        r.render_single(&rows()[0], to_dict).unwrap();
        assert_eq!(out.borrow().as_str(), "| id | 1 |\n");

        let (r, out) = rendering(OutputFormat::Yaml, OutputStyle::Markdown, false);
        r.render_single(&rows()[0], to_dict).unwrap();
        assert!(out.borrow().contains("description: first"));
    }

    #[test]
    fn print_writes_a_line() {
        let (r, out) = rendering(OutputFormat::Text, OutputStyle::Rounded, false);
        r.print("hello");
        assert_eq!(out.borrow().as_str(), "hello\n");
    }
}
