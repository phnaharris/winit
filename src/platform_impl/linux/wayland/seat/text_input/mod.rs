use std::ops::Deref;
use std::sync::MutexGuard;

use sctk::globals::GlobalData;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};

use sctk::reexports::client::delegate_dispatch;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::Dispatch;
use sctk::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use sctk::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_v3::Event as TextInputEvent;
use sctk::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use crate::event::{Ime, WindowEvent};
use crate::platform_impl::wayland;
use crate::platform_impl::wayland::state::WinitState;
use crate::window::ImePurpose;

pub struct TextInputState {
    text_input_manager: ZwpTextInputManagerV3,
}

impl TextInputState {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        let text_input_manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        Ok(Self { text_input_manager })
    }
}

impl Deref for TextInputState {
    type Target = ZwpTextInputManagerV3;

    fn deref(&self) -> &Self::Target {
        &self.text_input_manager
    }
}

impl Dispatch<ZwpTextInputManagerV3, GlobalData, WinitState> for TextInputState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpTextInputManagerV3,
        _event: <ZwpTextInputManagerV3 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, TextInputData, WinitState> for TextInputState {
    fn event(
        state: &mut WinitState,
        text_input: &ZwpTextInputV3,
        event: <ZwpTextInputV3 as Proxy>::Event,
        data: &TextInputData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let windows = state.windows.get_mut();
        let mut text_input_data = data.inner.lock().unwrap();
        match event {
            TextInputEvent::Enter { surface } => {
                println!("winit: Enter request");
                let window_id = wayland::make_wid(&surface);
                text_input_data.surface = Some(surface);

                let mut window = match windows.get(&window_id) {
                    Some(window) => window.lock().unwrap(),
                    None => return,
                };

                if window.ime_allowed() {
                    text_input.enable();
                    text_input.set_surrounding_text(" ".to_string(), 1, 1);
                    // text_input.set_text_change_cause(ChangeCause::InputMethod);
                    text_input.set_content_type_by_purpose(window.ime_purpose());
                    text_input.commit();
                    // commit_state(text_input, &mut text_input_data);
                    state
                        .events_sink
                        .push_window_event(WindowEvent::Ime(Ime::Enabled), window_id);
                }

                window.text_input_entered(text_input);
            }
            TextInputEvent::Leave { surface } => {
                println!("winit: Leave request");
                text_input_data.surface = None;

                // Always issue a disable.
                text_input.disable();
                text_input.commit();
                // commit_state(text_input, &mut text_input_data);

                let window_id = wayland::make_wid(&surface);

                // XXX this check is essential, because `leave` could have a
                // refence to nil surface...
                let mut window = match windows.get(&window_id) {
                    Some(window) => window.lock().unwrap(),
                    None => return,
                };

                window.text_input_left(text_input);

                state
                    .events_sink
                    .push_window_event(WindowEvent::Ime(Ime::Disabled), window_id);
            }
            TextInputEvent::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                println!("winit: PreeditString request");
                let text = text.unwrap_or_default();
                let cursor_begin = usize::try_from(cursor_begin)
                    .ok()
                    .and_then(|idx| text.is_char_boundary(idx).then_some(idx));
                let cursor_end = usize::try_from(cursor_end)
                    .ok()
                    .and_then(|idx| text.is_char_boundary(idx).then_some(idx));

                text_input_data.pending_preedit = Some(Preedit {
                    text,
                    cursor_begin,
                    cursor_end,
                })
            }
            TextInputEvent::CommitString { text } => {
                println!("winit: CommitString request");
                text_input_data.pending_preedit = None;
                text_input_data.pending_commit = text;
            }
            TextInputEvent::Done { .. } => {
                println!("winit: Done request");
                let window_id = match text_input_data.surface.as_ref() {
                    Some(surface) => wayland::make_wid(surface),
                    None => return,
                };

                state
                    .events_sink
                    .push_window_event(WindowEvent::Ime(Ime::RetrieveSurroundingText), window_id);

                // Clear preedit at the start of `Done`.
                state.events_sink.push_window_event(
                    WindowEvent::Ime(Ime::Preedit(String::new(), None)),
                    window_id,
                );

                if let Some(surrounding_delete) = text_input_data.pending_surrounding_delete.take()
                {
                    state.events_sink.push_window_event(
                        WindowEvent::Ime(Ime::DeleteSurroundingText {
                            before_length: surrounding_delete.before_length as usize,
                            after_length: surrounding_delete.after_length as usize,
                        }),
                        window_id,
                    );
                }

                // Send `Commit`.
                if let Some(text) = text_input_data.pending_commit.take() {
                    state.events_sink.push_window_event(
                        WindowEvent::Ime(Ime::Commit {
                            content: text,
                            selection: None,
                            compose_region: None,
                        }),
                        window_id,
                    );
                }

                // Send preedit.
                if let Some(preedit) = text_input_data.pending_preedit.take() {
                    let cursor_range = preedit
                        .cursor_begin
                        .map(|b| (b, preedit.cursor_end.unwrap_or(b)));

                    state.events_sink.push_window_event(
                        WindowEvent::Ime(Ime::Preedit(preedit.text, cursor_range)),
                        window_id,
                    );
                }
            }
            TextInputEvent::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                unimplemented!();
                // Not handled.
                println!("winit: DeleteSurroundingText request");
                text_input_data.pending_surrounding_delete = SurroundingDelete {
                    before_length,
                    after_length,
                }
                .into()
            }
            _ => {
                println!("winit: Something went wrong");
            }
        }
    }
}

pub trait ZwpTextInputV3Ext {
    fn set_content_type_by_purpose(&self, purpose: ImePurpose);
}

impl ZwpTextInputV3Ext for ZwpTextInputV3 {
    fn set_content_type_by_purpose(&self, purpose: ImePurpose) {
        let (hint, purpose) = match purpose {
            ImePurpose::Normal => (ContentHint::None, ContentPurpose::Normal),
            ImePurpose::Password => (ContentHint::SensitiveData, ContentPurpose::Password),
            ImePurpose::Terminal => (ContentHint::None, ContentPurpose::Terminal),
        };
        self.set_content_type(hint, purpose);
    }
}

/// The Data associated with the text input.
#[derive(Default)]
pub struct TextInputData {
    inner: std::sync::Mutex<TextInputDataInner>,
}

#[derive(Default)]
pub struct TextInputDataInner {
    /// The `WlSurface` we're performing input to.
    surface: Option<WlSurface>,

    /// The commit to submit on `done`.
    pending_commit: Option<String>,

    /// The preedit to submit on `done`.
    pending_preedit: Option<Preedit>,
    current_preedit: Option<Preedit>,

    pending_surrounding_delete: Option<SurroundingDelete>,
    surrounding: Option<Surrounding>,

    surrounding_change: Option<ChangeCause>,
}

/// The state of the preedit.
#[derive(Default)]
struct Preedit {
    text: String,
    cursor_begin: Option<usize>,
    cursor_end: Option<usize>,
}

#[derive(Default)]
pub struct Surrounding {
    pub text: String,
    pub cursor_idx: i32,
    pub anchor_idx: i32,
}

#[derive(Default)]
pub struct SurroundingDelete {
    before_length: u32,
    after_length: u32,
}

pub trait ZwpTextInputV3Applier {
    // fn delete_surrounding_text_apply(&self);
    // fn preedit_apply(&self);
    // fn surrounding_text_apply(&self);
    // fn commit_apply(&self);
}

impl ZwpTextInputV3Applier for ZwpTextInputV3 {
    // fn delete_surrounding_text_apply(&self) {
    //     todo!()
    // }

    // fn surrounding_text_apply(&self, text: String, cursor: i32, anchor: i32) {}

    // fn preedit_apply(&self) {
    //     todo!()
    // }

    // fn commit_apply(&self) {
    //     todo!()
    // }
}

// pub trait ZwpTextInputV3Notifier {
//     fn notify_surrounding_text(&self, surrounding: &Surrounding);
//     fn notify_cursor_location(&self);
//     fn notify_content_type(&self);
//     fn notify_im_change(
//         &self,
//         cause: Option<ChangeCause>,
//         data: &mut MutexGuard<'_, TextInputDataInner>,
//     );
//     fn commit_state(&mut self, data: &mut MutexGuard<'_, TextInputDataInner>);
// }

// impl ZwpTextInputV3Notifier for ZwpTextInputV3 {
//     fn notify_surrounding_text(&self, surrounding: &Surrounding) {
//         // TODO: calculate something
//         let Surrounding {
//             text,
//             cursor_idx: cursor,
//             anchor_idx: anchor,
//         } = surrounding;

//         self.set_surrounding_text(text.to_string(), *cursor, *anchor);
//     }

//     fn notify_cursor_location(&self) {
//         todo!()
//     }

//     fn notify_content_type(&self) {
//         todo!()
//     }

//     fn notify_im_change(
//         &self,
//         cause: Option<ChangeCause>,
//         data: &mut MutexGuard<'_, TextInputDataInner>,
//     ) {
//         // context->surrounding_change = cause;

//         // g_signal_emit_by_name (global->current, "retrieve-surrounding", &result);
//         // notify_surrounding_text (context);
//         // notify_content_type (context);
//         // notify_cursor_location (context);
//         // commit_state (context);

//         data.surrounding_change = cause;
//         self.notify_surrounding_text(data.surrounding.as_ref().unwrap());
//         self.notify_content_type();
//         self.notify_cursor_location();
//         // self.commit_state(text_input);
//     }

//     fn commit_state(&mut self, data: &mut MutexGuard<'_, TextInputDataInner>) {
//         // global->serial++;
//         // zwp_text_input_v3_commit (global->text_input);
//         // context->surrounding_change = ZWP_TEXT_INPUT_V3_CHANGE_CAUSE_INPUT_METHOD;
//         self.commit();
//         data.surrounding_change = Some(ChangeCause::InputMethod);
//     }
// }

fn notify_im_change(
    text_input: &ZwpTextInputV3,
    cause: &ChangeCause,
    data: &mut MutexGuard<'_, TextInputDataInner>,
) {
    data.surrounding_change = Some(*cause);
    retrieve_surrounding(data);
    notify_surrounding_text(text_input, data);
    notify_content_type(text_input, data);
    notify_cursor_location(text_input, data);
    // commit_state(text_input, data);
}

pub fn retrieve_surrounding(data: &mut MutexGuard<'_, TextInputDataInner>) {
    todo!()
}

fn notify_surrounding_text(
    text_input: &ZwpTextInputV3,
    data: &mut MutexGuard<'_, TextInputDataInner>,
) {
    todo!()
}

fn notify_content_type(text_input: &ZwpTextInputV3, data: &mut MutexGuard<'_, TextInputDataInner>) {
    todo!()
}

fn notify_cursor_location(
    text_input: &ZwpTextInputV3,
    data: &mut MutexGuard<'_, TextInputDataInner>,
) {
    todo!()
}

// fn commit_state(text_input: &ZwpTextInputV3, data: &mut MutexGuard<'_, TextInputDataInner>) {
//     text_input.commit();
//     data.surrounding_change = Some(ChangeCause::InputMethod);
// }

delegate_dispatch!(WinitState: [ZwpTextInputManagerV3: GlobalData] => TextInputState);
delegate_dispatch!(WinitState: [ZwpTextInputV3: TextInputData] => TextInputState);
