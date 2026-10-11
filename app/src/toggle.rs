//! An on/off switch that moves: the knob slides and the colours blend when
//! its value changes, where iced's own toggler jumps. It looks like that one
//! at rest.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::animation::Easing;
use iced::time::Instant;
use iced::widget::toggler::{self, Status};
use iced::{
    mouse, touch, window, Animation, Background, Border, Color, Element, Event, Length, Rectangle,
    Size, Theme,
};

/// How long the knob takes from one side to the other.
const TRAVEL: Duration = Duration::from_millis(180);

pub struct Toggle<'a, Message> {
    on: bool,
    on_toggle: Box<dyn Fn(bool) -> Message + 'a>,
    size: f32,
}

impl<'a, Message> Toggle<'a, Message> {
    pub fn new(on: bool, on_toggle: impl Fn(bool) -> Message + 'a) -> Self {
        Toggle {
            on,
            on_toggle: Box::new(on_toggle),
            size: 16.0,
        }
    }

    /// The height of the switch; it is twice as wide.
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

struct State {
    /// The side the knob is on or heading for, and how far it has got.
    knob: Animation<bool>,
    /// The time of the frame being drawn.
    now: Instant,
    hovered: bool,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Toggle<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            knob: Animation::new(self.on)
                .duration(TRAVEL)
                .easing(Easing::EaseInOutCubic),
            now: Instant::now(),
            hovered: false,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::new(2.0 * self.size, self.size))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let hovered = cursor.is_over(layout.bounds());
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. })
                if hovered =>
            {
                shell.publish((self.on_toggle)(!self.on));
                shell.capture_event();
            }
            // The value changes between frames, whoever changed it; each
            // frame of the way to the other side asks for the next one.
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.knob.go_mut(self.on, *now);
                state.now = *now;
                state.hovered = hovered;
                if state.knob.is_animating(*now) {
                    shell.request_redraw();
                }
            }
            _ if hovered != state.hovered => shell.request_redraw(),
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let look = |is_toggled| {
            toggler::default(
                theme,
                if state.hovered {
                    Status::Hovered { is_toggled }
                } else {
                    Status::Active { is_toggled }
                },
            )
        };
        let (off, on) = (look(false), look(true));
        // 0 with the knob on the left (off), 1 with it on the right.
        let way = state.knob.interpolate(0.0, 1.0, state.now);
        let round = Border::default().rounded(bounds.height / 2.0);

        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: round,
                ..renderer::Quad::default()
            },
            blend(off.background, on.background, way),
        );
        let padding = (on.padding_ratio * bounds.height).round();
        let knob = bounds.height - 2.0 * padding;
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x + padding + way * (bounds.width - bounds.height),
                    y: bounds.y + padding,
                    width: knob,
                    height: knob,
                },
                border: round,
                snap: false, // it slides by less than a pixel at a time
                ..renderer::Quad::default()
            },
            blend(off.foreground, on.foreground, way),
        );
    }
}

/// The colour `way` of the way from `from` to `to`.
fn blend(from: Background, to: Background, way: f32) -> Color {
    let colour = |background| match background {
        Background::Color(colour) => colour,
        Background::Gradient(_) => Color::TRANSPARENT,
    };
    let (from, to) = (colour(from), colour(to));
    let mix = |a: f32, b: f32| a + (b - a) * way;
    Color {
        r: mix(from.r, to.r),
        g: mix(from.g, to.g),
        b: mix(from.b, to.b),
        a: mix(from.a, to.a),
    }
}

impl<'a, Message: 'a> From<Toggle<'a, Message>> for Element<'a, Message> {
    fn from(toggle: Toggle<'a, Message>) -> Self {
        Element::new(toggle)
    }
}
