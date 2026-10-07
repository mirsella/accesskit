use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use accesskit::{
    Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, AriaCurrent,
    DeactivationHandler, HasPopup, Invalid, Live, NodeId as LocalNodeId, Orientation, Rect, Role,
    TextSelection, Toggled, TreeId, TreeUpdate,
};
use accesskit_consumer::{
    common_filter_with_root_exception, FilterResult, Node, NodeId, Tree, TreeChangeHandler,
};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{
    AbortController, AbortSignal, AddEventListenerOptions, CssStyleDeclaration, Document, Element,
    Event, EventTarget, FocusOptions, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    HtmlTextAreaElement, MutationObserver, MutationObserverInit, ResizeObserver,
};

use crate::{
    geometry::{map_rect, CssRect},
    roles::{dom_role, DomRole},
};

const NODE_KEY_ATTRIBUTE: &str = "data-accesskit-id";
const ROOT_ID: &str = "accesskit-root";

thread_local! {
    static RESERVED_ROOT_IDS: RefCell<Vec<(Document, HashSet<String>)>> = const { RefCell::new(Vec::new()) };
}

pub struct Adapter {
    state: Rc<RefCell<State>>,
}

struct State {
    canvas: HtmlCanvasElement,
    document: Document,
    root: HtmlElement,
    tree: Option<Tree>,
    elements: HashMap<u128, DomNode>,
    action_handler: Box<dyn ActionHandler>,
    deactivation_handler: Box<dyn DeactivationHandler>,
    syncing: bool,
    animation_frame_id: Option<i32>,
    animation_frame: Option<Closure<dyn FnMut()>>,
    resize_observer: Option<ResizeObserver>,
    mutation_observer: Option<MutationObserver>,
    mutation_callback: Option<Closure<dyn FnMut()>>,
    event_callback: Option<Closure<dyn FnMut(Event)>>,
    layout_callback: Option<Closure<dyn FnMut()>>,
    abort_controller: AbortController,
    warned_missing_root_bounds: bool,
}

struct DomNode {
    id: NodeId,
    element: HtmlElement,
    semantics: Semantics,
    native_range_value: Option<f64>,
    geometry: Option<CssRect>,
}

struct Semantics {
    dom_role: DomRole,
    attributes: Vec<(&'static str, String)>,
    text: Option<String>,
    value: Option<String>,
    numeric_value: Option<f64>,
    selection: Option<(u32, u32, &'static str)>,
    checked: Option<Toggled>,
}

struct Changed(bool);

impl TreeChangeHandler for Changed {
    fn node_added(&mut self, _node: &Node) {
        self.0 = true;
    }

    fn node_updated(&mut self, _old_node: &Node, _new_node: &Node) {
        self.0 = true;
    }

    fn focus_moved(&mut self, _old_node: Option<&Node>, _new_node: Option<&Node>) {
        self.0 = true;
    }

    fn node_removed(&mut self, _node: &Node) {
        self.0 = true;
    }
}

impl Adapter {
    pub fn new(
        canvas: HtmlCanvasElement,
        mut activation_handler: impl 'static + ActivationHandler,
        action_handler: impl 'static + ActionHandler,
        deactivation_handler: impl 'static + DeactivationHandler,
    ) -> Result<Self, JsValue> {
        let document = canvas
            .owner_document()
            .ok_or_else(|| JsValue::from_str("the canvas is not associated with a document"))?;
        let abort_controller = AbortController::new()?;

        let root = document.create_element("div")?.dyn_into::<HtmlElement>()?;
        root.style().set_css_text(
            "position:fixed;left:0;top:0;width:0;height:0;overflow:visible;pointer-events:none;z-index:1",
        );

        let initial_tree = activation_handler
            .request_initial_tree()
            .map(|update| Tree::new(update, true));
        root.set_id(&reserve_root_id(&document));
        let state = Rc::new(RefCell::new(State {
            canvas,
            document,
            root,
            tree: initial_tree,
            elements: HashMap::new(),
            action_handler: Box::new(action_handler),
            deactivation_handler: Box::new(deactivation_handler),
            syncing: false,
            animation_frame_id: None,
            animation_frame: None,
            resize_observer: None,
            mutation_observer: None,
            mutation_callback: None,
            event_callback: None,
            layout_callback: None,
            abort_controller,
            warned_missing_root_bounds: false,
        }));

        install_listeners(&state)?;
        sync_dom(&state)?;
        let placed = place_root(&state.borrow())?;
        if placed {
            sync_focus(&state);
        }
        schedule_geometry(&state);
        Ok(Self { state })
    }

    pub fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        let update = updater();
        let changed = {
            let mut state = self.state.borrow_mut();
            if let Some(tree) = &mut state.tree {
                let mut changed = Changed(false);
                tree.update_and_process_changes(update, &mut changed);
                changed.0
            } else {
                state.tree = Some(Tree::new(update, true));
                true
            }
        };

        if let Err(error) = sync_dom(&self.state) {
            web_sys::console::error_1(&error);
            return;
        }
        if changed {
            schedule_geometry(&self.state);
            sync_focus(&self.state);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.abort_controller.abort();
        if let Some(id) = self.animation_frame_id.take() {
            if let Some(window) = web_sys::window() {
                let _ = window.cancel_animation_frame(id);
            }
        }
        if let Some(observer) = self.resize_observer.take() {
            observer.disconnect();
        }
        if let Some(observer) = self.mutation_observer.take() {
            observer.disconnect();
        }
        self.root.remove();
        release_root_id(&self.document, &self.root.id());
        self.deactivation_handler.deactivate_accessibility();
    }
}

fn install_listeners(state: &Rc<RefCell<State>>) -> Result<(), JsValue> {
    let weak = Rc::downgrade(state);
    let event_callback = Closure::new(move |event: Event| {
        if let Some(state) = weak.upgrade() {
            handle_event(&state, event);
        }
    });
    let signal = state.borrow().abort_controller.signal();
    add_listeners(
        state.borrow().root.as_ref(),
        &["focusin", "focusout", "click", "keydown", "input", "select"],
        event_callback.as_ref(),
        &signal,
    )?;
    state.borrow_mut().event_callback = Some(event_callback);

    let weak = Rc::downgrade(state);
    let animation_frame = Closure::new(move || {
        if let Some(state) = weak.upgrade() {
            state.borrow_mut().animation_frame_id = None;
            if let Err(error) = sync_geometry(&state) {
                web_sys::console::error_1(&error);
            }
        }
    });
    state.borrow_mut().animation_frame = Some(animation_frame);

    let weak = Rc::downgrade(state);
    let layout_callback = Closure::new(move || {
        if let Some(state) = weak.upgrade() {
            schedule_geometry(&state);
        }
    });
    let observer = ResizeObserver::new(layout_callback.as_ref().unchecked_ref())?;
    observer.observe(&state.borrow().canvas);
    state.borrow_mut().resize_observer = Some(observer);

    let weak = Rc::downgrade(state);
    let mutation_callback = Closure::new(move || {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let placed = place_root(&state.borrow());
        match placed {
            Ok(true) => {
                schedule_geometry(&state);
                sync_focus(&state);
            }
            Ok(false) => {}
            Err(error) => web_sys::console::error_1(&error),
        }
    });
    let observer = MutationObserver::new(mutation_callback.as_ref().unchecked_ref())?;
    let options = MutationObserverInit::new();
    options.set_child_list(true);
    options.set_subtree(true);
    observer.observe_with_options(&state.borrow().document, &options)?;
    {
        let mut state = state.borrow_mut();
        state.mutation_observer = Some(observer);
        state.mutation_callback = Some(mutation_callback);
    }

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window is unavailable"))?;
    add_listeners(
        window.as_ref(),
        &["resize", "scroll"],
        layout_callback.as_ref(),
        &signal,
    )?;
    if let Some(viewport) = window.visual_viewport() {
        add_listeners(
            viewport.as_ref(),
            &["resize", "scroll"],
            layout_callback.as_ref(),
            &signal,
        )?;
    }
    state.borrow_mut().layout_callback = Some(layout_callback);
    Ok(())
}

fn reserve_root_id(document: &Document) -> String {
    RESERVED_ROOT_IDS.with(|documents| {
        let mut documents = documents.borrow_mut();
        let ids = if let Some((_, ids)) = documents.iter_mut().find(|(owner, _)| owner == document)
        {
            ids
        } else {
            documents.push((document.clone(), HashSet::new()));
            &mut documents.last_mut().expect("document was inserted").1
        };
        let mut suffix = 0_u64;
        loop {
            let id = if suffix == 0 {
                ROOT_ID.to_owned()
            } else {
                format!("{ROOT_ID}-{suffix}")
            };
            if !ids.contains(&id) && document.get_element_by_id(&id).is_none() {
                ids.insert(id.clone());
                return id;
            }
            suffix = suffix.checked_add(1).expect("too many web adapters");
        }
    })
}

fn release_root_id(document: &Document, id: &str) {
    RESERVED_ROOT_IDS.with(|documents| {
        let mut documents = documents.borrow_mut();
        let index = documents
            .iter()
            .position(|(owner, _)| owner == document)
            .expect("root ID document must remain reserved");
        assert!(
            documents[index].1.remove(id),
            "root ID must remain reserved"
        );
        if documents[index].1.is_empty() {
            documents.swap_remove(index);
        }
    });
}

fn place_root(state: &State) -> Result<bool, JsValue> {
    if !state.canvas.is_connected() {
        state.root.remove();
        return Ok(false);
    }
    let parent = state
        .canvas
        .parent_node()
        .expect("a connected canvas must have a parent");
    let next = state.canvas.next_sibling();
    if next
        .as_ref()
        .is_some_and(|next| state.root.is_same_node(Some(next)))
    {
        return Ok(false);
    }
    parent.insert_before(&state.root, next.as_ref())?;
    Ok(true)
}

fn add_listeners(
    target: &EventTarget,
    names: &[&str],
    callback: &JsValue,
    signal: &AbortSignal,
) -> Result<(), JsValue> {
    let options = AddEventListenerOptions::new();
    options.set_signal(signal);
    for name in names {
        target.add_event_listener_with_callback_and_add_event_listener_options(
            name,
            callback.unchecked_ref(),
            &options,
        )?;
    }
    Ok(())
}

fn schedule_geometry(state: &Rc<RefCell<State>>) {
    let mut state_ref = state.borrow_mut();
    if state_ref.animation_frame_id.is_some() {
        return;
    }
    let Some(callback) = &state_ref.animation_frame else {
        return;
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Ok(id) = window.request_animation_frame(callback.as_ref().unchecked_ref()) {
        state_ref.animation_frame_id = Some(id);
    }
}

fn web_filter(node: &Node) -> FilterResult {
    if let Some(dialog) = node.tree_state.active_dialog().filter(Node::is_modal) {
        if !node.is_descendant_of(&dialog) && !dialog.is_descendant_of(node) {
            return FilterResult::ExcludeSubtree;
        }
    }

    // Keep inactive panels as hidden relationship targets for their tabs.
    // Their descendants remain excluded by the ordinary hidden-node filter.
    if node.role() == Role::TabPanel && node.is_hidden() {
        return FilterResult::Include;
    }
    let common = common_filter_with_root_exception(node);
    if common != FilterResult::Include {
        return common;
    }
    if node.is_root() || node.is_focused() {
        return FilterResult::Include;
    }

    let meaningful = node.label().is_some()
        || node.description().is_some()
        || node.value().is_some()
        || node.live() != Live::Off
        || [
            Action::Click,
            Action::Focus,
            Action::SetValue,
            Action::SetTextSelection,
            Action::Increment,
            Action::Decrement,
        ]
        .into_iter()
        .any(|action| node.supports_action(action, &common_filter_with_root_exception));
    match node.role() {
        Role::Unknown | Role::Pane | Role::Canvas if !meaningful => FilterResult::ExcludeNode,
        Role::Image if node.label().is_none() && node.description().is_none() => {
            FilterResult::ExcludeSubtree
        }
        Role::Label | Role::TextRun if !meaningful => FilterResult::ExcludeNode,
        _ => FilterResult::Include,
    }
}

fn sync_dom(state: &Rc<RefCell<State>>) -> Result<(), JsValue> {
    let root_id = {
        let state = state.borrow();
        state.tree.as_ref().map(|tree| tree.state().root().id())
    };
    let Some(root_id) = root_id else {
        return Ok(());
    };

    let mut stale = state
        .borrow()
        .elements
        .keys()
        .copied()
        .collect::<HashSet<_>>();
    sync_subtree(state, root_id, None, 0, &mut stale)?;
    let stale_nodes = {
        let mut state = state.borrow_mut();
        let elements = &mut state.elements;
        stale
            .into_iter()
            .map(|key| {
                elements
                    .remove(&key)
                    .expect("stale DOM node must remain retained")
            })
            .collect::<Vec<_>>()
    };
    for node in stale_nodes {
        node.element.remove();
    }
    Ok(())
}

fn sync_subtree(
    state: &Rc<RefCell<State>>,
    id: NodeId,
    parent: Option<NodeId>,
    index: usize,
    stale: &mut HashSet<u128>,
) -> Result<(), JsValue> {
    let (semantics, children) = {
        let state_ref = state.borrow();
        let tree = state_ref
            .tree
            .as_ref()
            .expect("tree was checked by sync_dom");
        let node = tree
            .state()
            .node_by_id(id)
            .expect("filtered child must belong to the retained tree");
        (
            semantics_for(tree, &node, &state_ref.root.id()),
            node.filtered_children(web_filter)
                .map(|child| child.id())
                .collect::<Vec<_>>(),
        )
    };

    stale.remove(&u128::from(id));
    ensure_dom_node(state, id, parent, index, semantics)?;
    for (index, child) in children.into_iter().enumerate() {
        sync_subtree(state, child, Some(id), index, stale)?;
    }
    Ok(())
}

fn ensure_dom_node(
    state: &Rc<RefCell<State>>,
    id: NodeId,
    parent: Option<NodeId>,
    index: usize,
    semantics: Semantics,
) -> Result<(), JsValue> {
    let key = u128::from(id);
    let existing = state.borrow_mut().elements.remove(&key);
    let mut node = match existing {
        Some(node)
            if node.semantics.dom_role.tag == semantics.dom_role.tag
                && node.semantics.dom_role.input_type == semantics.dom_role.input_type =>
        {
            node
        }
        Some(node) => {
            node.element.remove();
            create_dom_node(state, id, &semantics)?
        }
        None => create_dom_node(state, id, &semantics)?,
    };
    let expected_parent: Element = {
        let state_ref = state.borrow();
        match parent {
            Some(parent) => state_ref
                .elements
                .get(&u128::from(parent))
                .expect("retained child must have a retained parent")
                .element
                .clone()
                .unchecked_into(),
            None => state_ref.root.clone().unchecked_into(),
        }
    };
    state.borrow_mut().syncing = true;
    let result = apply_semantics(&mut node, semantics);
    state.borrow_mut().syncing = false;
    let element = node.element.clone();
    state.borrow_mut().elements.insert(key, node);
    result?;
    let before = expected_parent.child_nodes().item(index as u32);
    if before
        .as_ref()
        .is_none_or(|before| !element.is_same_node(Some(before)))
    {
        expected_parent.insert_before(&element, before.as_ref())?;
    }
    Ok(())
}

fn create_dom_node(
    state: &Rc<RefCell<State>>,
    id: NodeId,
    semantics: &Semantics,
) -> Result<DomNode, JsValue> {
    let state_ref = state.borrow();
    let tree = state_ref.tree.as_ref().expect("DOM sync requires a tree");
    let (local_id, tree_id) = tree
        .state()
        .locate_node(id)
        .expect("DOM node must belong to the retained tree");
    let element = state_ref
        .document
        .create_element(semantics.dom_role.tag)?
        .dyn_into::<HtmlElement>()?;
    element.set_id(&dom_id(&state_ref.root.id(), tree_id, local_id));
    element.set_attribute(NODE_KEY_ATTRIBUTE, &u128::from(id).to_string())?;
    element.style().set_css_text(
        "position:fixed;box-sizing:border-box;margin:0;padding:0;border:0;opacity:0;color:transparent;background:transparent;pointer-events:none",
    );

    Ok(DomNode {
        id,
        element,
        semantics: Semantics {
            dom_role: semantics.dom_role,
            attributes: Vec::new(),
            text: None,
            value: None,
            numeric_value: None,
            selection: None,
            checked: None,
        },
        native_range_value: semantics.numeric_value,
        geometry: None,
    })
}

fn handle_event(state: &Rc<RefCell<State>>, event: Event) {
    let Some((id, dom_role)) = event_node(state, &event) else {
        return;
    };
    let event_type = event.type_();
    match event_type.as_str() {
        "focusin" | "focusout" if !state.borrow().syncing => {
            let action = if event_type == "focusin" {
                Action::Focus
            } else {
                Action::Blur
            };
            dispatch_action(state, id, action, None);
        }
        "click" => {
            dispatch_action(state, id, Action::Click, None);
        }
        "keydown" if !matches!(dom_role.tag, "button" | "input" | "textarea") => {
            let Some(keyboard) = event.dyn_ref::<web_sys::KeyboardEvent>() else {
                return;
            };
            if !keyboard.alt_key() && !keyboard.ctrl_key() && !keyboard.meta_key() {
                let target = {
                    let state_ref = state.borrow();
                    state_ref
                        .tree
                        .as_ref()
                        .and_then(|tree| tree.state().node_by_id(id))
                        .and_then(|node| tab_keyboard_target(&node, &keyboard.key()))
                };
                if let Some(target) = target {
                    dispatch_action(state, target, Action::Focus, None);
                    dispatch_action(state, target, Action::Click, None);
                    keyboard.prevent_default();
                    return;
                }
            }
            if keyboard.repeat() || !matches!(keyboard.key().as_str(), "Enter" | " ") {
                return;
            }
            if dispatch_action(state, id, Action::Click, None) {
                keyboard.prevent_default();
            }
        }
        "input" | "select" if !state.borrow().syncing => {
            if event_type == "input" {
                let data = event.target().and_then(|target| {
                    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
                        if dom_role.input_type == Some("range") {
                            dispatch_range_action(state, id, input);
                            None
                        } else {
                            Some(ActionData::Value(input.value().into_boxed_str()))
                        }
                    } else {
                        target
                            .dyn_ref::<HtmlTextAreaElement>()
                            .map(|textarea| ActionData::Value(textarea.value().into_boxed_str()))
                    }
                });
                if let Some(data) = data {
                    dispatch_action(state, id, Action::SetValue, Some(data));
                }
            }
            dispatch_selection(state, id, &event);
        }
        _ => {}
    }
}

fn tab_keyboard_target(node: &Node, key: &str) -> Option<NodeId> {
    if node.role() != Role::Tab || node.is_disabled() {
        return None;
    }
    let mut parent = node.parent();
    let list = loop {
        let ancestor = parent?;
        if ancestor.role() == Role::TabList {
            break ancestor;
        }
        parent = ancestor.parent();
    };
    let vertical = list.orientation() == Some(Orientation::Vertical);
    let tabs = || {
        list.filtered_children(web_filter)
            .filter(|tab| tab.role() == Role::Tab && !tab.is_disabled())
            .map(|tab| tab.id())
    };
    let backward = match key {
        "Home" => return tabs().next(),
        "End" => return tabs().next_back(),
        "ArrowRight" if !vertical => false,
        "ArrowLeft" if !vertical => true,
        "ArrowDown" if vertical => false,
        "ArrowUp" if vertical => true,
        _ => return None,
    };
    let mut remaining = tabs();
    if backward {
        remaining.rfind(|id| *id == node.id())?;
        remaining.next_back().or_else(|| tabs().next_back())
    } else {
        remaining.find(|id| *id == node.id())?;
        remaining.next().or_else(|| tabs().next())
    }
}

fn dispatch_range_action(state: &Rc<RefCell<State>>, id: NodeId, input: &HtmlInputElement) {
    let value = input.value_as_number();
    let action = {
        let state_ref = state.borrow();
        let tree = state_ref
            .tree
            .as_ref()
            .expect("a retained DOM node requires a tree");
        let node = tree
            .state()
            .node_by_id(id)
            .expect("range event node must remain in the retained tree");
        if node.supports_action(Action::SetValue, &common_filter_with_root_exception) {
            Action::SetValue
        } else {
            let Some(current) = state_ref
                .elements
                .get(&u128::from(id))
                .expect("range event node must remain retained")
                .native_range_value
            else {
                return;
            };
            let action = if value > current {
                Action::Increment
            } else if value < current {
                Action::Decrement
            } else {
                input.set_value(&current.to_string());
                return;
            };
            if !node.supports_action(action, &common_filter_with_root_exception) {
                input.set_value(&current.to_string());
                return;
            }
            action
        }
    };
    state
        .borrow_mut()
        .elements
        .get_mut(&u128::from(id))
        .expect("range event node must remain retained")
        .native_range_value = Some(value);
    let data = (action == Action::SetValue).then_some(ActionData::NumericValue(value));
    assert!(dispatch_action(state, id, action, data));
}

fn event_node(state: &Rc<RefCell<State>>, event: &Event) -> Option<(NodeId, DomRole)> {
    let key = event
        .target()?
        .dyn_ref::<Element>()?
        .get_attribute(NODE_KEY_ATTRIBUTE)?
        .parse()
        .ok()?;
    state
        .borrow()
        .elements
        .get(&key)
        .map(|node| (node.id, node.semantics.dom_role))
}

fn dispatch_selection(state: &Rc<RefCell<State>>, id: NodeId, event: &Event) {
    let Some(target) = event.target() else {
        return;
    };
    let selection = if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        let Ok(Some(start)) = input.selection_start() else {
            return;
        };
        let Ok(Some(end)) = input.selection_end() else {
            return;
        };
        (
            start,
            end,
            input.selection_direction().ok().flatten().as_deref() == Some("backward"),
        )
    } else if let Some(textarea) = target.dyn_ref::<HtmlTextAreaElement>() {
        let Ok(Some(start)) = textarea.selection_start() else {
            return;
        };
        let Ok(Some(end)) = textarea.selection_end() else {
            return;
        };
        (
            start,
            end,
            textarea.selection_direction().ok().flatten().as_deref() == Some("backward"),
        )
    } else {
        return;
    };

    let data = {
        let state_ref = state.borrow();
        let Some(tree) = &state_ref.tree else {
            return;
        };
        let Some(node) = tree.state().node_by_id(id) else {
            return;
        };
        let start = selection.0 as usize;
        let end = selection.1 as usize;
        let (anchor, focus) = if selection.2 {
            (end, start)
        } else {
            (start, end)
        };
        let Some(anchor) = node.text_position_from_global_utf16_index(anchor) else {
            return;
        };
        let Some(focus) = node.text_position_from_global_utf16_index(focus) else {
            return;
        };
        ActionData::SetTextSelection(TextSelection {
            anchor: anchor.to_raw(),
            focus: focus.to_raw(),
        })
    };
    dispatch_action(state, id, Action::SetTextSelection, Some(data));
}

fn dispatch_action(
    state: &Rc<RefCell<State>>,
    id: NodeId,
    action: Action,
    data: Option<ActionData>,
) -> bool {
    let request = {
        let state_ref = state.borrow();
        let Some(tree) = &state_ref.tree else {
            return false;
        };
        let Some(node) = tree.state().node_by_id(id) else {
            return false;
        };
        if !node.supports_action(action, &common_filter_with_root_exception) {
            return false;
        }
        let Some((target_node, target_tree)) = tree.state().locate_node(id) else {
            return false;
        };
        ActionRequest {
            action,
            target_tree,
            target_node,
            data,
        }
    };
    state.borrow_mut().action_handler.do_action(request);
    true
}

fn apply_semantics(node: &mut DomNode, semantics: Semantics) -> Result<(), JsValue> {
    node.native_range_value = semantics.numeric_value;
    for (name, _) in &node.semantics.attributes {
        if !semantics
            .attributes
            .iter()
            .any(|(new_name, _)| new_name == name)
        {
            node.element.remove_attribute(name)?;
        }
    }
    for (name, value) in &semantics.attributes {
        if node
            .semantics
            .attributes
            .iter()
            .find(|(old_name, _)| old_name == name)
            .is_none_or(|(_, old_value)| old_value != value)
        {
            node.element.set_attribute(name, value)?;
        }
    }
    if node.semantics.text != semantics.text {
        node.element.set_text_content(semantics.text.as_deref());
    }
    sync_native_text(node, &semantics)?;
    if let Some(input) = node.element.dyn_ref::<HtmlInputElement>() {
        input.set_checked(semantics.checked == Some(Toggled::True));
        input.set_indeterminate(semantics.checked == Some(Toggled::Mixed));
    }
    node.semantics = semantics;
    Ok(())
}

fn sync_native_text(node: &DomNode, semantics: &Semantics) -> Result<(), JsValue> {
    let Some(value) = &semantics.value else {
        return Ok(());
    };
    if let Some(input) = node.element.dyn_ref::<HtmlInputElement>() {
        if input.value() != *value {
            input.set_value(value);
        }
        if matches!(
            semantics.dom_role.input_type,
            Some("text" | "search" | "password" | "tel" | "url")
        ) {
            if let Some((start, end, direction)) = semantics.selection {
                input.set_selection_range_with_direction(start, end, direction)?;
            }
        }
    } else if let Some(textarea) = node.element.dyn_ref::<HtmlTextAreaElement>() {
        if textarea.value() != *value {
            textarea.set_value(value);
        }
        if let Some((start, end, direction)) = semantics.selection {
            textarea.set_selection_range_with_direction(start, end, direction)?;
        }
    }
    Ok(())
}

fn semantics_for(tree: &Tree, node: &Node, root_id: &str) -> Semantics {
    let dom_role = dom_role(node.role());
    let (_, tree_id) = tree
        .state()
        .locate_node(node.id())
        .expect("consumer node must belong to its tree");
    let data = node.data();
    let mut attributes = Vec::new();
    if let Some(role) = dom_role.aria_role {
        attribute(&mut attributes, "role", role);
    }
    if dom_role.tag == "button" {
        attribute(&mut attributes, "type", "button");
    }
    if let Some(input_type) = dom_role.input_type {
        attribute(&mut attributes, "type", input_type);
    }
    if node.is_hidden() {
        attribute(&mut attributes, "hidden", "");
    }
    if node.role() == Role::EmailInput {
        attribute(&mut attributes, "inputmode", "email");
        attribute(&mut attributes, "autocomplete", "email");
    }

    optional_attribute(&mut attributes, "aria-label", node.label());
    let described_by = relation_ids(root_id, tree_id, data.described_by());
    if described_by.is_empty() {
        optional_attribute(&mut attributes, "aria-description", node.description());
    } else {
        attribute(&mut attributes, "aria-describedby", &described_by);
    }
    for (name, ids) in [
        ("aria-labelledby", data.labelled_by()),
        ("aria-controls", data.controls()),
        ("aria-details", data.details()),
        ("aria-owns", data.owns()),
    ] {
        let value = relation_ids(root_id, tree_id, ids);
        if !value.is_empty() {
            attribute(&mut attributes, name, &value);
        }
    }
    optional_attribute(
        &mut attributes,
        "aria-roledescription",
        node.role_description(),
    );

    if node.is_disabled() {
        attribute(&mut attributes, "disabled", "");
        attribute(&mut attributes, "aria-disabled", "true");
    }
    if node.is_read_only() && node.is_text_input() {
        attribute(&mut attributes, "readonly", "");
        attribute(&mut attributes, "aria-readonly", "true");
    }
    if node.is_required() {
        attribute(&mut attributes, "required", "");
        attribute(&mut attributes, "aria-required", "true");
    }
    for (name, value) in [
        ("aria-selected", node.is_selected()),
        ("aria-expanded", data.is_expanded()),
    ] {
        if let Some(value) = value {
            attribute(&mut attributes, name, if value { "true" } else { "false" });
        }
    }
    if node.is_modal() {
        attribute(&mut attributes, "aria-modal", "true");
    }
    if node.is_busy() {
        attribute(&mut attributes, "aria-busy", "true");
    }
    if node.is_multiselectable() {
        attribute(&mut attributes, "aria-multiselectable", "true");
    }
    if node.is_live_atomic() {
        attribute(&mut attributes, "aria-atomic", "true");
    }
    optional_attribute(
        &mut attributes,
        "aria-invalid",
        data.invalid().map(invalid_value),
    );
    optional_attribute(
        &mut attributes,
        "aria-orientation",
        node.orientation().map(orientation_value),
    );
    optional_attribute(
        &mut attributes,
        "aria-current",
        node.aria_current().map(current_value),
    );
    optional_attribute(
        &mut attributes,
        "aria-haspopup",
        node.has_popup().map(popup_value),
    );
    match node.live() {
        Live::Off => {}
        Live::Polite => attribute(&mut attributes, "aria-live", "polite"),
        Live::Assertive => attribute(&mut attributes, "aria-live", "assertive"),
    }
    if let Some(toggled) = node.toggled() {
        let name = if matches!(node.role(), Role::Button | Role::DefaultButton) {
            "aria-pressed"
        } else {
            "aria-checked"
        };
        attribute(&mut attributes, name, toggled_value(toggled));
    }
    numeric_attribute(&mut attributes, "aria-valuenow", node.numeric_value());
    numeric_attribute(&mut attributes, "aria-valuemin", node.min_numeric_value());
    numeric_attribute(&mut attributes, "aria-valuemax", node.max_numeric_value());
    if dom_role.input_type == Some("range") {
        numeric_attribute(&mut attributes, "min", node.min_numeric_value());
        numeric_attribute(&mut attributes, "max", node.max_numeric_value());
        numeric_attribute(&mut attributes, "step", node.numeric_value_step());
    }
    if !node.is_text_input() {
        optional_attribute(&mut attributes, "aria-valuetext", node.value());
    }
    if let Some(level) = node.level() {
        attribute(&mut attributes, "aria-level", &level.to_string());
    }
    if let Some(placeholder) = node.placeholder() {
        attribute(&mut attributes, "placeholder", placeholder);
    }

    let focusable = !node.is_disabled()
        && (node.role() != Role::Tab || node.is_selected() == Some(true))
        && (node.is_focusable(&common_filter_with_root_exception)
            || node.is_clickable(&common_filter_with_root_exception)
            || node.is_text_input());
    attribute(
        &mut attributes,
        "tabindex",
        if focusable { "0" } else { "-1" },
    );

    let value = if node.is_text_input() {
        Some(node.value().unwrap_or_default())
    } else if node.role() == Role::Slider {
        node.numeric_value().map(|value| value.to_string())
    } else {
        None
    };
    let selection = node
        .text_selection_anchor()
        .zip(node.text_selection_focus())
        .map(|(anchor, focus)| {
            let anchor = anchor.to_global_utf16_index() as u32;
            let focus = focus.to_global_utf16_index() as u32;
            if anchor <= focus {
                (anchor, focus, "forward")
            } else {
                (focus, anchor, "backward")
            }
        });
    let text = if matches!(
        node.role(),
        Role::Label
            | Role::TextRun
            | Role::Paragraph
            | Role::Heading
            | Role::Status
            | Role::Alert
            | Role::Timer
    ) {
        node.value().or_else(|| node.label())
    } else {
        None
    };
    Semantics {
        dom_role,
        attributes,
        text,
        value,
        numeric_value: node.numeric_value(),
        selection,
        checked: node.toggled(),
    }
}

fn relation_ids(root_id: &str, tree_id: TreeId, ids: &[LocalNodeId]) -> String {
    ids.iter()
        .map(|id| dom_id(root_id, tree_id, *id))
        .collect::<Vec<_>>()
        .join(" ")
}

fn dom_id(root_id: &str, tree_id: TreeId, node_id: LocalNodeId) -> String {
    format!("{root_id}-node-{}-{}", tree_id.0, node_id.0)
}

fn attribute(attributes: &mut Vec<(&'static str, String)>, name: &'static str, value: &str) {
    attributes.push((name, value.to_owned()));
}

fn optional_attribute(
    attributes: &mut Vec<(&'static str, String)>,
    name: &'static str,
    value: Option<impl AsRef<str>>,
) {
    if let Some(value) = value {
        attribute(attributes, name, value.as_ref());
    }
}

fn numeric_attribute(
    attributes: &mut Vec<(&'static str, String)>,
    name: &'static str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        attribute(attributes, name, &value.to_string());
    }
}

fn invalid_value(value: Invalid) -> &'static str {
    match value {
        Invalid::True => "true",
        Invalid::Grammar => "grammar",
        Invalid::Spelling => "spelling",
    }
}

fn orientation_value(value: Orientation) -> &'static str {
    match value {
        Orientation::Horizontal => "horizontal",
        Orientation::Vertical => "vertical",
    }
}

fn current_value(value: AriaCurrent) -> &'static str {
    match value {
        AriaCurrent::False => "false",
        AriaCurrent::True => "true",
        AriaCurrent::Page => "page",
        AriaCurrent::Step => "step",
        AriaCurrent::Location => "location",
        AriaCurrent::Date => "date",
        AriaCurrent::Time => "time",
    }
}

fn popup_value(value: HasPopup) -> &'static str {
    match value {
        HasPopup::Menu => "menu",
        HasPopup::Listbox => "listbox",
        HasPopup::Tree => "tree",
        HasPopup::Grid => "grid",
        HasPopup::Dialog => "dialog",
    }
}

fn toggled_value(value: Toggled) -> &'static str {
    match value {
        Toggled::False => "false",
        Toggled::True => "true",
        Toggled::Mixed => "mixed",
    }
}

fn sync_focus(state: &Rc<RefCell<State>>) {
    let element = {
        let state_ref = state.borrow();
        state_ref
            .tree
            .as_ref()
            .and_then(|tree| tree.state().focus())
            .filter(|node| !node.is_root())
            .filter(|node| node.is_focusable(&common_filter_with_root_exception))
            .and_then(|node| state_ref.elements.get(&u128::from(node.id())))
            .map(|node| node.element.clone())
    };
    let Some(element) = element else {
        blur_semantic_focus(state);
        return;
    };

    {
        let state_ref = state.borrow();
        if let Some(active) = state_ref.document.active_element() {
            if active.is_same_node(Some(&element)) {
                return;
            }
            if !state_ref.root.contains(Some(&active))
                && !active.is_same_node(Some(&state_ref.canvas))
                && active.tag_name() != "BODY"
            {
                return;
            }
        }
    }

    state.borrow_mut().syncing = true;
    let options = FocusOptions::new();
    options.set_prevent_scroll(true);
    let _ = element.focus_with_options(&options);
    state.borrow_mut().syncing = false;
}

fn blur_semantic_focus(state: &Rc<RefCell<State>>) {
    let active = {
        let state_ref = state.borrow();
        state_ref
            .document
            .active_element()
            .filter(|active| state_ref.root.contains(Some(active)))
            .and_then(|active| active.dyn_into::<HtmlElement>().ok())
    };
    let Some(active) = active else {
        return;
    };
    state.borrow_mut().syncing = true;
    let _ = active.blur();
    state.borrow_mut().syncing = false;
}

fn sync_geometry(state: &Rc<RefCell<State>>) -> Result<(), JsValue> {
    let mut state_ref = state.borrow_mut();
    let Some(tree) = &state_ref.tree else {
        return Ok(());
    };
    let rect = state_ref.canvas.get_bounding_client_rect();
    let canvas_rect = CssRect {
        left: rect.left(),
        top: rect.top(),
        width: rect.width(),
        height: rect.height(),
    };
    let root_bounds = tree.state().root().bounding_box();
    let logical = root_bounds.unwrap_or_else(|| {
        if !state_ref.warned_missing_root_bounds {
            web_sys::console::warn_1(&JsValue::from_str(
                "AccessKit web root has no bounds; using canvas backing-store dimensions",
            ));
            state_ref.warned_missing_root_bounds = true;
        }
        Rect::new(
            0.0,
            0.0,
            f64::from(state_ref.canvas.width()),
            f64::from(state_ref.canvas.height()),
        )
    });
    set_rect(&state_ref.root.style(), canvas_rect)?;

    let State { tree, elements, .. } = &mut *state_ref;
    let tree = tree.as_ref().expect("tree was checked above");
    for node in elements.values_mut() {
        let retained = tree
            .state()
            .node_by_id(node.id)
            .expect("retained DOM node must belong to the retained tree");
        let geometry = clipped_bounds(&retained)
            .and_then(|bounds| map_rect(bounds, logical, canvas_rect))
            // Boundless live regions still need an exposed, nonempty CSS box.
            .unwrap_or(CssRect {
                left: canvas_rect.left,
                top: canvas_rect.top,
                width: 1.0,
                height: 1.0,
            });
        if node.geometry == Some(geometry) {
            continue;
        }
        set_rect(&node.element.style(), geometry)?;
        node.geometry = Some(geometry);
    }
    Ok(())
}

fn set_rect(style: &CssStyleDeclaration, rect: CssRect) -> Result<(), JsValue> {
    style.set_property("left", &format!("{}px", rect.left))?;
    style.set_property("top", &format!("{}px", rect.top))?;
    style.set_property("width", &format!("{}px", rect.width))?;
    style.set_property("height", &format!("{}px", rect.height))?;
    Ok(())
}

fn clipped_bounds(node: &Node) -> Option<Rect> {
    let mut bounds = node.bounding_box()?;
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if ancestor.clips_children() {
            bounds = bounds.intersect(ancestor.bounding_box()?);
            if bounds.is_empty() {
                return None;
            }
        }
        parent = ancestor.parent();
    }
    Some(bounds)
}

#[cfg(test)]
mod browser_tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        task::Poll,
    };

    use accesskit::{
        Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler,
        Node, NodeId, Rect, Role, Tree as TreeData, TreeId, TreeUpdate,
    };
    use accesskit_consumer::{FilterResult, Tree as ConsumerTree};
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::*;
    use web_sys::{Event, EventInit, HtmlCanvasElement, HtmlElement, HtmlInputElement};

    use super::{sync_geometry, web_filter, Adapter, LocalNodeId, ROOT_ID};

    wasm_bindgen_test_configure!(run_in_browser);

    struct InitialTree;

    impl ActivationHandler for InitialTree {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            Some(initial_tree())
        }
    }

    struct SliderTree;

    struct TabsTree;

    impl ActivationHandler for TabsTree {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            let root_id = LocalNodeId(0);
            let mut root = accesskit::Node::new(Role::RootWebArea);
            root.set_children(vec![
                LocalNodeId(1),
                LocalNodeId(5),
                LocalNodeId(6),
                LocalNodeId(7),
            ]);
            let mut list = accesskit::Node::new(Role::TabList);
            list.set_children(vec![LocalNodeId(2), LocalNodeId(3), LocalNodeId(4)]);
            let mut nodes = vec![(root_id, root), (LocalNodeId(1), list)];
            for (index, label) in ["Deck", "Battle", "Leaderboard"].iter().enumerate() {
                let id = LocalNodeId(index as u64 + 2);
                let panel_id = LocalNodeId(index as u64 + 5);
                let mut tab = accesskit::Node::new(Role::Tab);
                tab.set_label(*label);
                tab.set_selected(index == 1);
                tab.set_controls(vec![panel_id]);
                tab.add_action(Action::Focus);
                tab.add_action(Action::Click);
                let mut panel = accesskit::Node::new(Role::TabPanel);
                panel.set_labelled_by(vec![id]);
                panel.add_action(Action::Focus);
                if index != 1 {
                    panel.set_hidden();
                }
                nodes.extend([(id, tab), (panel_id, panel)]);
            }
            Some(TreeUpdate {
                nodes,
                tree: Some(TreeData::new(root_id)),
                tree_id: TreeId::ROOT,
                focus: root_id,
            })
        }
    }

    #[wasm_bindgen_test]
    fn tabs_preserve_relationships_and_route_keyboard_navigation() {
        let document = web_sys::window().unwrap().document().unwrap();
        let canvas = document
            .create_element("canvas")
            .unwrap()
            .dyn_into::<HtmlCanvasElement>()
            .unwrap();
        document.body().unwrap().append_child(&canvas).unwrap();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let adapter = Adapter::new(
            canvas.clone(),
            TabsTree,
            Actions(Rc::clone(&actions)),
            Deactivation,
        )
        .unwrap();
        let root = adapter.state.borrow().root.clone();
        let tabs = root.query_selector_all("[role=tab]").unwrap();
        for index in 0..3 {
            let tab = tabs.item(index).unwrap().dyn_into::<HtmlElement>().unwrap();
            assert_eq!(tab.tab_index(), if index == 1 { 0 } else { -1 });
            let panel = document
                .get_element_by_id(&tab.get_attribute("aria-controls").unwrap())
                .unwrap();
            assert_eq!(panel.get_attribute("aria-labelledby"), Some(tab.id()));
            assert_eq!(panel.has_attribute("hidden"), index != 1);
        }
        for (index, key, target) in [
            (1, "ArrowRight", 4),
            (1, "ArrowLeft", 2),
            (1, "Home", 2),
            (1, "End", 4),
            (0, "ArrowLeft", 4),
            (2, "ArrowRight", 2),
        ] {
            let init = web_sys::KeyboardEventInit::new();
            init.set_key(key);
            init.set_bubbles(true);
            init.set_cancelable(true);
            let event = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                .unwrap();
            tabs.item(index).unwrap().dispatch_event(&event).unwrap();
            assert!(event.default_prevented());
            let requests = actions.borrow();
            assert_eq!(requests[requests.len() - 2].action, Action::Focus);
            assert_eq!(requests.last().unwrap().action, Action::Click);
            assert_eq!(requests.last().unwrap().target_node, LocalNodeId(target));
        }
        drop(adapter);
        canvas.remove();
    }

    impl ActivationHandler for SliderTree {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            let mut root = Node::new(Role::RootWebArea);
            root.set_children(vec![NodeId(1), NodeId(2), NodeId(3)]);
            let mut directional = Node::new(Role::Slider);
            directional.set_numeric_value(5.0);
            directional.add_action(Action::Increment);
            directional.add_action(Action::Decrement);
            let mut set_value = Node::new(Role::Slider);
            set_value.set_numeric_value(5.0);
            set_value.add_action(Action::SetValue);
            let mut increment_only = Node::new(Role::Slider);
            increment_only.set_numeric_value(5.0);
            increment_only.add_action(Action::Increment);
            Some(TreeUpdate {
                nodes: vec![
                    (NodeId(0), root),
                    (NodeId(1), directional),
                    (NodeId(2), set_value),
                    (NodeId(3), increment_only),
                ],
                tree: Some(TreeData::new(NodeId(0))),
                tree_id: TreeId::ROOT,
                focus: NodeId(0),
            })
        }
    }

    struct Activation(Rc<Cell<usize>>);

    impl ActivationHandler for Activation {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            self.0.set(self.0.get() + 1);
            None
        }
    }

    struct Actions(Rc<RefCell<Vec<ActionRequest>>>);

    impl ActionHandler for Actions {
        fn do_action(&mut self, request: ActionRequest) {
            self.0.borrow_mut().push(request);
        }
    }

    struct Deactivation;

    impl DeactivationHandler for Deactivation {
        fn deactivate_accessibility(&mut self) {}
    }

    fn initial_tree() -> TreeUpdate {
        let mut root = Node::new(Role::RootWebArea);
        root.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));
        root.set_children(vec![NodeId(2)]);
        let mut container = Node::new(Role::GenericContainer);
        container.set_children(vec![NodeId(1)]);
        let mut button = Node::new(Role::Button);
        button.set_label("Start");
        button.set_bounds(Rect::new(10.0, 20.0, 60.0, 50.0));
        button.add_action(Action::Click);
        button.add_action(Action::Focus);
        button.add_action(Action::Blur);
        TreeUpdate {
            nodes: vec![
                (NodeId(0), root),
                (NodeId(1), button),
                (NodeId(2), container),
            ],
            tree: Some(TreeData::new(NodeId(0))),
            tree_id: TreeId::ROOT,
            focus: NodeId(0),
        }
    }

    async fn next_microtask() {
        let mut first = true;
        std::future::poll_fn(|cx| {
            if std::mem::take(&mut first) {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }

    fn input_range(input: &HtmlInputElement, value: &str) {
        input.set_value(value);
        let event = EventInit::new();
        event.set_bubbles(true);
        input
            .dispatch_event(&Event::new_with_event_init_dict("input", &event).unwrap())
            .unwrap();
    }

    #[wasm_bindgen_test]
    fn retains_focused_empty_nodes() {
        let mut root = Node::new(Role::RootWebArea);
        root.set_children(vec![NodeId(1)]);
        let tree = ConsumerTree::new(
            TreeUpdate {
                nodes: vec![(NodeId(0), root), (NodeId(1), Node::new(Role::Unknown))],
                tree: Some(TreeData::new(NodeId(0))),
                tree_id: TreeId::ROOT,
                focus: NodeId(1),
            },
            true,
        );

        assert_eq!(
            web_filter(&tree.state().root().children().next().unwrap()),
            FilterResult::Include
        );
    }

    #[wasm_bindgen_test]
    fn maps_range_input_to_supported_actions() {
        let document = web_sys::window().unwrap().document().unwrap();
        let canvas = document
            .create_element("canvas")
            .unwrap()
            .dyn_into::<HtmlCanvasElement>()
            .unwrap();
        document.body().unwrap().append_child(&canvas).unwrap();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut adapter = Adapter::new(
            canvas.clone(),
            SliderTree,
            Actions(actions.clone()),
            Deactivation,
        )
        .unwrap();
        let tree_root = canvas
            .next_element_sibling()
            .unwrap()
            .first_element_child()
            .unwrap();
        let directional = tree_root.first_element_child().unwrap();
        let set_value = directional.next_element_sibling().unwrap();
        let increment_only = set_value.next_element_sibling().unwrap();
        let directional = directional.dyn_into::<HtmlInputElement>().unwrap();
        let set_value = set_value.dyn_into::<HtmlInputElement>().unwrap();
        let increment_only = increment_only.dyn_into::<HtmlInputElement>().unwrap();

        input_range(&directional, "6");
        input_range(&directional, "7");
        input_range(&directional, "6");
        input_range(&set_value, "7");
        input_range(&increment_only, "4");
        assert_eq!(increment_only.value(), "5");
        assert_eq!(actions.borrow().len(), 4);
        input_range(&increment_only, "5");
        assert_eq!(actions.borrow().len(), 4);
        input_range(&increment_only, "6");

        adapter.update_if_active(|| TreeUpdate {
            nodes: Vec::new(),
            tree: None,
            tree_id: TreeId::ROOT,
            focus: NodeId(0),
        });
        assert_eq!(directional.value(), "5");
        assert_eq!(set_value.value(), "5");
        assert_eq!(increment_only.value(), "5");

        let actions = actions.borrow();
        assert_eq!(actions.len(), 5);
        assert_eq!(actions[0].action, Action::Increment);
        assert_eq!(actions[0].target_node, NodeId(1));
        assert_eq!(actions[0].data, None);
        assert_eq!(actions[1].action, Action::Increment);
        assert_eq!(actions[1].target_node, NodeId(1));
        assert_eq!(actions[1].data, None);
        assert_eq!(actions[2].action, Action::Decrement);
        assert_eq!(actions[2].target_node, NodeId(1));
        assert_eq!(actions[2].data, None);
        assert_eq!(actions[3].action, Action::SetValue);
        assert_eq!(actions[3].target_node, NodeId(2));
        assert_eq!(actions[3].data, Some(ActionData::NumericValue(7.0)));
        assert_eq!(actions[4].action, Action::Increment);
        assert_eq!(actions[4].target_node, NodeId(3));
        assert_eq!(actions[4].data, None);

        drop(actions);
        drop(adapter);
        canvas.remove();
    }

    #[wasm_bindgen_test]
    async fn attaches_populated_detached_root_and_follows_canvas() {
        let document = web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .implementation()
            .unwrap()
            .create_html_document()
            .unwrap();
        let canvas = document
            .create_element("canvas")
            .unwrap()
            .dyn_into::<HtmlCanvasElement>()
            .unwrap();
        let activations = Rc::new(Cell::new(0));
        let mut adapter = Adapter::new(
            canvas.clone(),
            Activation(activations.clone()),
            Actions(Rc::new(RefCell::new(Vec::new()))),
            Deactivation,
        )
        .unwrap();
        let root = adapter.state.borrow().root.clone();
        assert_eq!(root.id(), ROOT_ID);
        assert!(!root.is_connected());
        assert!(root.first_element_child().is_none());
        assert_eq!(activations.get(), 1);

        adapter.update_if_active(initial_tree);
        assert!(root.first_element_child().is_some());

        let second_canvas = canvas
            .clone_node()
            .unwrap()
            .dyn_into::<HtmlCanvasElement>()
            .unwrap();
        let second_adapter = Adapter::new(
            second_canvas,
            InitialTree,
            Actions(Rc::new(RefCell::new(Vec::new()))),
            Deactivation,
        )
        .unwrap();
        assert_eq!(second_adapter.state.borrow().root.id(), "accesskit-root-1");

        document.body().unwrap().append_child(&canvas).unwrap();
        next_microtask().await;
        assert_eq!(canvas.next_element_sibling().unwrap().id(), root.id());

        let container = document.create_element("div").unwrap();
        document.body().unwrap().append_child(&container).unwrap();
        container.append_child(&canvas).unwrap();
        next_microtask().await;
        assert_eq!(canvas.next_element_sibling().unwrap().id(), root.id());

        canvas.remove();
        next_microtask().await;
        assert!(!root.is_connected());
        assert_eq!(activations.get(), 1);

        drop(adapter);
        let replacement = Adapter::new(
            canvas.clone(),
            InitialTree,
            Actions(Rc::new(RefCell::new(Vec::new()))),
            Deactivation,
        )
        .unwrap();
        assert_eq!(replacement.state.borrow().root.id(), ROOT_ID);

        drop(replacement);
        drop(second_adapter);
        container.remove();
    }

    #[wasm_bindgen_test]
    fn creates_stable_incremental_dom_and_cleans_up() {
        let document = web_sys::window().unwrap().document().unwrap();
        let canvas = document
            .create_element("canvas")
            .unwrap()
            .dyn_into::<HtmlCanvasElement>()
            .unwrap();
        canvas.set_width(100);
        canvas.set_height(100);
        canvas.style().set_property("width", "100px").unwrap();
        canvas.style().set_property("height", "100px").unwrap();
        document.body().unwrap().append_child(&canvas).unwrap();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut adapter = Adapter::new(
            canvas.clone(),
            InitialTree,
            Actions(actions.clone()),
            Deactivation,
        )
        .unwrap();
        let root = canvas.next_element_sibling().unwrap();
        assert_eq!(root.tag_name(), "DIV");
        assert_eq!(root.id(), ROOT_ID);
        let button = root
            .first_element_child()
            .unwrap()
            .first_element_child()
            .unwrap();
        assert!(button
            .parent_element()
            .unwrap()
            .is_same_node(root.first_element_child().as_deref()));
        assert_eq!(button.get_attribute("aria-label").as_deref(), Some("Start"));
        assert_eq!(button.get_attribute("value"), None);
        let button_element = button.clone().dyn_into::<HtmlElement>().unwrap();
        assert_eq!(
            button_element
                .style()
                .get_property_value("pointer-events")
                .unwrap(),
            "none"
        );
        sync_geometry(&adapter.state).unwrap();
        assert_eq!(
            button_element.style().get_property_value("width").unwrap(),
            "50px"
        );
        button_element.focus().unwrap();
        button_element.click();
        button_element.blur().unwrap();
        assert_eq!(
            actions
                .borrow()
                .iter()
                .map(|request| request.action)
                .collect::<Vec<_>>(),
            [Action::Focus, Action::Click, Action::Blur]
        );

        let mut changed = Node::new(Role::Button);
        changed.set_label("Continue");
        changed.set_bounds(Rect::new(10.0, 20.0, 60.0, 50.0));
        adapter.update_if_active(|| TreeUpdate {
            nodes: vec![(NodeId(1), changed)],
            tree: None,
            tree_id: TreeId::ROOT,
            focus: NodeId(0),
        });
        let updated = document.get_element_by_id(&button.id()).unwrap();
        assert!(button.is_same_node(Some(&updated)));
        assert_eq!(
            updated.get_attribute("aria-label").as_deref(),
            Some("Continue")
        );

        canvas.set_width(200);
        canvas.set_height(200);
        let mut root_without_bounds = Node::new(Role::RootWebArea);
        root_without_bounds.set_children(vec![NodeId(2)]);
        adapter.update_if_active(|| TreeUpdate {
            nodes: vec![(NodeId(0), root_without_bounds)],
            tree: None,
            tree_id: TreeId::ROOT,
            focus: NodeId(0),
        });
        sync_geometry(&adapter.state).unwrap();
        assert_eq!(
            button_element.style().get_property_value("width").unwrap(),
            "25px"
        );

        let second_canvas = canvas
            .clone_node()
            .unwrap()
            .dyn_into::<HtmlCanvasElement>()
            .unwrap();
        document
            .body()
            .unwrap()
            .append_child(&second_canvas)
            .unwrap();
        let second_adapter = Adapter::new(
            second_canvas.clone(),
            InitialTree,
            Actions(Rc::new(RefCell::new(Vec::new()))),
            Deactivation,
        )
        .unwrap();
        let second_root = second_canvas.next_element_sibling().unwrap();
        assert_eq!(second_root.id(), "accesskit-root-1");
        assert_ne!(
            root.first_element_child().unwrap().id(),
            second_root.first_element_child().unwrap().id()
        );
        drop(second_adapter);
        second_canvas.remove();

        button_element.focus().unwrap();
        assert!(document
            .active_element()
            .unwrap()
            .is_same_node(Some(&button)));
        let action_count = actions.borrow().len();
        let mut replacement_root = Node::new(Role::RootWebArea);
        replacement_root.set_label("Replacement");
        replacement_root.set_bounds(Rect::new(0.0, 0.0, 100.0, 100.0));
        adapter.update_if_active(|| TreeUpdate {
            nodes: vec![(NodeId(3), replacement_root)],
            tree: Some(TreeData::new(NodeId(3))),
            tree_id: TreeId::ROOT,
            focus: NodeId(3),
        });
        assert!(!button.is_connected());
        assert_eq!(actions.borrow().len(), action_count);
        assert_eq!(
            root.first_element_child()
                .unwrap()
                .get_attribute("aria-label")
                .as_deref(),
            Some("Replacement")
        );

        drop(adapter);
        assert!(!root.is_connected());
        let replacement = Adapter::new(
            canvas.clone(),
            InitialTree,
            Actions(Rc::new(RefCell::new(Vec::new()))),
            Deactivation,
        )
        .unwrap();
        assert_eq!(canvas.next_element_sibling().unwrap().id(), ROOT_ID);
        drop(replacement);
        canvas.remove();
    }
}
