//! A small app to try the bridge on: an input, buttons, a dropdown (an
//! overlay), a checkbox list in a scrollable and a counter.
//!
//! `ICED_MCP=1 cargo run -p iced-mcp --example todos`, then point an agent at
//! the `iced-mcp` server (or `launch` it from there). Fixtures: `empty`, and
//! `groceries` (three items, the second done); `ICED_MCP_FIXTURE=groceries`
//! starts in one.

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Element, Fill};

#[derive(Debug, Default)]
struct Todos {
    draft: String,
    items: Vec<(String, bool)>,
    filter: Filter,
    clicks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Open,
    Done,
}

impl Filter {
    const ALL: [Filter; 3] = [Filter::All, Filter::Open, Filter::Done];
}

impl std::fmt::Display for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Filter::All => "All",
            Filter::Open => "Open",
            Filter::Done => "Done",
        })
    }
}

#[derive(Debug, Clone)]
enum Message {
    DraftChanged(String),
    Add,
    Toggle(usize, bool),
    FilterPicked(Filter),
    Count,
    Seed(String),
}

impl Todos {
    fn update(&mut self, message: Message) {
        match message {
            Message::DraftChanged(draft) => self.draft = draft,
            Message::Add => {
                let item = self.draft.trim().to_owned();
                if !item.is_empty() {
                    self.items.push((item, false));
                }
                self.draft.clear();
            }
            Message::Toggle(index, done) => {
                if let Some(item) = self.items.get_mut(index) {
                    item.1 = done;
                }
            }
            Message::FilterPicked(filter) => self.filter = filter,
            Message::Count => self.clicks += 1,
            Message::Seed(text) => self.items.push((text, false)),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let input = text_input("What needs doing?", &self.draft)
            .id("new-todo")
            .on_input(Message::DraftChanged)
            .on_submit(Message::Add);

        let items = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, (_, done))| match self.filter {
                Filter::All => true,
                Filter::Open => !done,
                Filter::Done => *done,
            })
            .map(|(index, (label, done))| {
                checkbox(*done)
                    .label(label.as_str())
                    .on_toggle(move |done| Message::Toggle(index, done))
                    .into()
            });

        let list = scrollable(column(items).spacing(6).width(Fill))
            .id("todo-list")
            .height(220);

        column![
            text("Todos").size(28),
            row![input, button("Add").on_press(Message::Add)].spacing(8),
            row![
                text("Show"),
                pick_list(Filter::ALL, Some(self.filter), Message::FilterPicked),
                button(text(format!("Clicked {} times", self.clicks))).on_press(Message::Count),
            ]
            .spacing(8),
            container(list).padding(8),
            bars::Bars::new(&self.items),
            text(format!(
                "{} items, {} done",
                self.items.len(),
                self.items.iter().filter(|i| i.1).count()
            )),
        ]
        .spacing(12)
        .padding(20)
        .into()
    }
}

fn main() -> iced::Result {
    let app = iced::application(Todos::default, Todos::update, Todos::view).title("Todos");
    if !iced_mcp::enabled() {
        return app.run();
    }
    // What agents get beyond the screen: the state, and named actions.
    iced_mcp::attach(app)
        .state(|todos| format!("{todos:#?}"))
        .query(
            "items",
            "The todos with their done flag. Args: {\"done\": bool} to filter (optional).",
            |todos, args| {
                let only = args.get("done").and_then(serde_json::Value::as_bool);
                let items: Vec<_> = todos
                    .items
                    .iter()
                    .filter(|(_, done)| only.is_none_or(|d| d == *done))
                    .map(|(text, done)| serde_json::json!({ "text": text, "done": done }))
                    .collect();
                Ok(serde_json::json!(items))
            },
        )
        // Named starting states for tests: each replaces the whole state.
        .fixture("empty", "No todos. No args.", |_| Ok(Todos::default()))
        .fixture(
            "groceries",
            "Three todos (milk, eggs done, bread). Args: {\"filter\": \"All\"|\"Open\"|\"Done\"} (optional).",
            |args| {
                let filter = match args.get("filter").and_then(|f| f.as_str()) {
                    None | Some("All") => Filter::All,
                    Some("Open") => Filter::Open,
                    Some("Done") => Filter::Done,
                    Some(other) => return Err(format!("unknown filter {other:?}")),
                };
                Ok(Todos {
                    items: vec![
                        ("milk".into(), false),
                        ("eggs".into(), true),
                        ("bread".into(), false),
                    ],
                    filter,
                    ..Todos::default()
                })
            },
        )
        .action(
            "add_todo",
            "Adds a todo. Args: {\"text\": string}.",
            |args| {
                let text = args
                    .get("text")
                    .and_then(|t| t.as_str())
                    .ok_or("needs {\"text\": string}")?;
                Ok(Message::Seed(text.to_owned()))
            },
        )
        .run()
}

/// A custom-drawn widget — one bar per todo, like a tiny chart — that
/// describes what it draws for agents with the semantics convention. Nothing
/// else in the tree reports these bars: without the description, an agent
/// would see one empty box.
mod bars {
    use iced::advanced::layout::{self, Layout};
    use iced::advanced::renderer::{self, Renderer as _};
    use iced::advanced::widget::{Operation, Tree, Widget};
    use iced::{mouse, Color, Element, Length, Rectangle, Size, Theme};
    use iced_mcp::protocol::{Rect, SemanticNode};

    const HEIGHT: f32 = 60.0;
    const BAR: f32 = 18.0;
    const GAP: f32 = 6.0;

    pub struct Bars {
        items: Vec<(String, bool)>,
    }

    impl Bars {
        pub fn new(items: &[(String, bool)]) -> Self {
            Self {
                items: items.to_vec(),
            }
        }

        fn bar(&self, bounds: Rectangle, index: usize) -> Rectangle {
            let (label, _) = &self.items[index];
            let h = (label.len() as f32 * 4.0).clamp(6.0, HEIGHT);
            Rectangle::new(
                iced::Point::new(bounds.x + index as f32 * (BAR + GAP), bounds.y + HEIGHT - h),
                Size::new(BAR, h),
            )
        }
    }

    impl<Message> Widget<Message, Theme, iced::Renderer> for Bars {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fixed(HEIGHT))
        }

        fn layout(
            &mut self,
            _: &mut Tree,
            _: &iced::Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            layout::Node::new(limits.resolve(Length::Fill, HEIGHT, Size::ZERO))
        }

        fn draw(
            &self,
            _: &Tree,
            renderer: &mut iced::Renderer,
            _: &Theme,
            _: &renderer::Style,
            layout: Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
            for (index, (_, done)) in self.items.iter().enumerate() {
                let color = if *done {
                    Color::from_rgb(0.3, 0.7, 0.4)
                } else {
                    Color::from_rgb(0.4, 0.5, 0.9)
                };
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: self.bar(layout.bounds(), index),
                        ..renderer::Quad::default()
                    },
                    color,
                );
            }
        }

        fn operate(
            &mut self,
            _: &mut Tree,
            layout: Layout<'_>,
            _: &iced::Renderer,
            operation: &mut dyn Operation,
        ) {
            let nodes: Vec<SemanticNode> = self
                .items
                .iter()
                .enumerate()
                .map(|(index, (label, done))| {
                    let r = self.bar(layout.bounds(), index);
                    SemanticNode {
                        role: "bar".into(),
                        name: label.clone(),
                        value: Some(if *done { "done" } else { "open" }.into()),
                        description: None,
                        placeholder: None,
                        disabled: None,
                        checked: None,
                        indeterminate: None,
                        selected: None,
                        expanded: None,
                        choices: None,
                        numeric_value: None,
                        min_numeric_value: None,
                        max_numeric_value: None,
                        numeric_value_step: None,
                        track_bounds: None,
                        vertical: None,
                        inverted: None,
                        id: Some(format!("bar-{index}")),
                        bounds: Rect {
                            x: r.x,
                            y: r.y,
                            width: r.width,
                            height: r.height,
                        },
                        children: Vec::new(),
                    }
                })
                .collect();
            let mut payload = SemanticNode::payload(&nodes);
            operation.custom(Some(&"todo-bars".into()), layout.bounds(), &mut payload);
        }
    }

    impl<'a, Message: 'a> From<Bars> for Element<'a, Message> {
        fn from(bars: Bars) -> Self {
            Element::new(bars)
        }
    }
}
