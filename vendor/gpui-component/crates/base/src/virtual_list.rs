//! Virtual List for render a large number of differently sized rows/columns.
//!
//! > NOTE: This must ensure each column width or row height.
//!
//! Only visible range are rendered for performance reasons.
//!
//! Inspired by `gpui::uniform_list`.
//! https://github.com/zed-industries/zed/blob/0ae1603610ab6b265bdfbee7b8dbc23c5ab06edc/crates/gpui/src/elements/uniform_list.rs
//!
//! Unlike the `uniform_list`, the each item can have different size.
//!
//! This is useful for more complex layout, for example, a table with different row height.
use std::{
    cell::RefCell,
    cmp,
    ops::{Deref, Range},
    rc::{Rc, Weak},
};

use gpui::{
    Along, AnyElement, App, AvailableSpace, Axis, Bounds, ContentMask, Context,
    DeferredScrollToItem, Div, Element, ElementId, Entity, GlobalElementId, Half, Hitbox,
    InteractiveElement, IntoElement, IsZero as _, ListSizingBehavior, Pixels, Point, Render,
    ScrollHandle, ScrollStrategy, Size, Stateful, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, div, point, px, size,
};
use smallvec::SmallVec;

use crate::{AxisExt, InteractiveElementExt as _};

struct VirtualListScrollHandleState {
    axis: Axis,
    items_count: usize,
    /// Content bounds size (excluding padding and border) from the last frame.
    last_content_size: Option<Size<Pixels>>,
    item_layout: Option<Rc<RefCell<ItemSizeLayout>>>,
    pub deferred_scroll_to_item: Option<DeferredScrollToItem>,
}

/// A scroll handle for [`VirtualList`].
///
/// See also [`ScrollHandle`].
#[derive(Clone)]
pub struct VirtualListScrollHandle {
    state: Rc<RefCell<VirtualListScrollHandleState>>,
    base_handle: ScrollHandle,
}

impl From<ScrollHandle> for VirtualListScrollHandle {
    fn from(handle: ScrollHandle) -> Self {
        let mut this = VirtualListScrollHandle::new();
        this.base_handle = handle;
        this
    }
}

impl AsRef<ScrollHandle> for VirtualListScrollHandle {
    fn as_ref(&self) -> &ScrollHandle {
        &self.base_handle
    }
}

impl crate::ScrollbarHandle for VirtualListScrollHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.base_handle.bounds()
    }

    fn offset(&self) -> Point<Pixels> {
        self.base_handle.offset()
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.base_handle.set_offset(offset);
    }

    fn content_size(&self) -> Size<Pixels> {
        self.base_handle.content_size()
    }
}

impl Deref for VirtualListScrollHandle {
    type Target = ScrollHandle;

    fn deref(&self) -> &Self::Target {
        &self.base_handle
    }
}

impl VirtualListScrollHandle {
    /// Create a new VirtualListScrollHandle.
    pub fn new() -> Self {
        VirtualListScrollHandle {
            state: Rc::new(RefCell::new(VirtualListScrollHandleState {
                axis: Axis::Vertical,
                items_count: 0,
                last_content_size: None,
                item_layout: None,
                deferred_scroll_to_item: None,
            })),
            base_handle: ScrollHandle::default(),
        }
    }

    /// Get the base scroll handle.
    pub fn base_handle(&self) -> &ScrollHandle {
        &self.base_handle
    }

    /// 返回最近布局中条目的起始坐标，index 等于条目数时返回内容末端。
    pub fn item_offset(&self, index: usize) -> Option<Pixels> {
        let state = self.state.borrow();
        let layout = state.item_layout.as_ref()?.borrow();
        if index == layout.sizes.len() {
            Some(layout.content_size.along(state.axis))
        } else {
            layout.origins.get(index).copied()
        }
    }

    /// Scroll to the item at the given index.
    pub fn scroll_to_item(&self, ix: usize, strategy: ScrollStrategy) {
        self.scroll_to_item_with_offset(ix, strategy, 0);
    }

    /// Scroll to the item at the given index, with an additional offset items.
    fn scroll_to_item_with_offset(&self, ix: usize, strategy: ScrollStrategy, offset: usize) {
        let mut state = self.state.borrow_mut();
        state.deferred_scroll_to_item = Some(DeferredScrollToItem {
            item_index: ix,
            strategy,
            offset,
            scroll_strict: false,
        });
    }

    /// Scrolls to the bottom of the list.
    pub fn scroll_to_bottom(&self) {
        let items_count = self.state.borrow().items_count;
        self.scroll_to_item(items_count.saturating_sub(1), ScrollStrategy::Top);
    }
}

/// Create a [`VirtualList`] in vertical direction.
///
/// This is like `uniform_list` in GPUI, but support two axis.
///
/// The `item_sizes` is the size of each row. Only the `height` is used; `width` is inferred
/// by measuring the item selected with [`VirtualList::with_item_to_measure_index`], defaulting
/// to the first item.
///
/// See also [`h_virtual_list`]
#[inline]
pub fn v_virtual_list<R, V>(
    view: Entity<V>,
    id: impl Into<ElementId>,
    item_sizes: Rc<Vec<Size<Pixels>>>,
    f: impl 'static + Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<R>,
) -> VirtualList
where
    R: IntoElement,
    V: Render,
{
    virtual_list(view, id, Axis::Vertical, item_sizes, f)
}

/// 创建支持局部重排行高更新的纵向虚拟列表。
#[inline]
pub fn v_virtual_list_reorderable<R, V>(
    view: Entity<V>,
    id: impl Into<ElementId>,
    item_sizes: ReorderableVirtualListSizes,
    f: impl 'static + Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<R>,
) -> VirtualList
where
    R: IntoElement,
    V: Render,
{
    let id: ElementId = id.into();
    let scroll_handle = VirtualListScrollHandle::new();
    let items_count = item_sizes.len();
    let render_range = move |visible_range, window: &mut Window, cx: &mut App| {
        view.update(cx, |this, cx| {
            f(this, visible_range, window, cx)
                .into_iter()
                .map(|component| component.into_any_element())
                .collect()
        })
    };

    VirtualList {
        id: id.clone(),
        axis: Axis::Vertical,
        base: div()
            .id(id)
            .size_full()
            .overflow_scroll()
            .lock_scroll_axis()
            .track_scroll(&scroll_handle),
        scroll_handle,
        items_count,
        item_sizes: Rc::new(Vec::new()),
        reorderable_item_sizes: Some(item_sizes),
        render_items: Box::new(render_range),
        sizing_behavior: ListSizingBehavior::default(),
        item_to_measure_index: 0,
        measure_cross_axis: true,
    }
}

/// Create a [`VirtualList`] in horizontal direction.
///
/// The `item_sizes` is the size of each column. Only the `width` is used; `height` is inferred
/// by measuring the item selected with [`VirtualList::with_item_to_measure_index`], defaulting
/// to the first item.
///
/// See also [`v_virtual_list`]
#[inline]
pub fn h_virtual_list<R, V>(
    view: Entity<V>,
    id: impl Into<ElementId>,
    item_sizes: Rc<Vec<Size<Pixels>>>,
    f: impl 'static + Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<R>,
) -> VirtualList
where
    R: IntoElement,
    V: Render,
{
    virtual_list(view, id, Axis::Horizontal, item_sizes, f)
}

#[doc(hidden)]
pub fn virtual_list<R, V>(
    view: Entity<V>,
    id: impl Into<ElementId>,
    axis: Axis,
    item_sizes: Rc<Vec<Size<Pixels>>>,
    f: impl 'static + Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<R>,
) -> VirtualList
where
    R: IntoElement,
    V: Render,
{
    let id: ElementId = id.into();
    let scroll_handle = VirtualListScrollHandle::new();
    let render_range = move |visible_range, window: &mut Window, cx: &mut App| {
        view.update(cx, |this, cx| {
            f(this, visible_range, window, cx)
                .into_iter()
                .map(|component| component.into_any_element())
                .collect()
        })
    };

    VirtualList {
        id: id.clone(),
        axis,
        base: div()
            .id(id)
            .size_full()
            .overflow_scroll()
            .lock_scroll_axis()
            .track_scroll(&scroll_handle),
        scroll_handle,
        items_count: item_sizes.len(),
        item_sizes,
        reorderable_item_sizes: None,
        render_items: Box::new(render_range),
        sizing_behavior: ListSizingBehavior::default(),
        item_to_measure_index: 0,
        measure_cross_axis: true,
    }
}

/// VirtualList component for rendering a large number of differently sized items.
pub struct VirtualList {
    id: ElementId,
    axis: Axis,
    base: Stateful<Div>,
    scroll_handle: VirtualListScrollHandle,
    items_count: usize,
    item_sizes: Rc<Vec<Size<Pixels>>>,
    reorderable_item_sizes: Option<ReorderableVirtualListSizes>,
    render_items: Box<
        dyn for<'a> Fn(Range<usize>, &'a mut Window, &'a mut App) -> SmallVec<[AnyElement; 64]>,
    >,
    sizing_behavior: ListSizingBehavior,
    item_to_measure_index: usize,
    measure_cross_axis: bool,
}

#[derive(Clone, Default)]
pub struct ReorderableVirtualListSizes {
    state: Rc<RefCell<ReorderableVirtualListSizesState>>,
}

#[derive(Default)]
struct ReorderableVirtualListSizesState {
    values: Vec<Size<Pixels>>,
    version: u64,
    dirty_range: Option<Range<usize>>,
}

impl ReorderableVirtualListSizes {
    pub fn new(values: Vec<Size<Pixels>>) -> Self {
        Self {
            state: Rc::new(RefCell::new(ReorderableVirtualListSizesState {
                values,
                version: 0,
                dirty_range: None,
            })),
        }
    }

    pub fn len(&self) -> usize {
        self.state.borrow().values.len()
    }

    pub fn set_size(&self, index: usize, size: Size<Pixels>) {
        let mut state = self.state.borrow_mut();
        let Some(value) = state.values.get_mut(index) else {
            return;
        };
        if *value == size {
            return;
        }
        *value = size;
        state.dirty_range = Some(match state.dirty_range.take() {
            Some(previous) => previous.start.min(index)..previous.end.max(index + 1),
            None => index..index + 1,
        });
        state.version = state.version.wrapping_add(1);
    }

    pub fn move_item(&self, from: usize, to: usize) {
        if from == to {
            return;
        }

        let mut state = self.state.borrow_mut();
        if from >= state.values.len() || to >= state.values.len() {
            return;
        }
        if from < to {
            state.values[from..=to].rotate_left(1);
        } else {
            state.values[to..=from].rotate_right(1);
        }

        let changed_range = from.min(to)..from.max(to) + 1;
        state.dirty_range = Some(match state.dirty_range.take() {
            Some(previous) => {
                previous.start.min(changed_range.start)..previous.end.max(changed_range.end)
            }
            None => changed_range,
        });
        state.version = state.version.wrapping_add(1);
    }
}

fn rebuild_item_size_layout(
    state: &mut ItemSizeLayout,
    values: &[Size<Pixels>],
    axis: Axis,
    gap: Pixels,
    items_count: usize,
) {
    state.axis = Some(axis);
    state.gap = gap;
    state.sizes = values
        .iter()
        .enumerate()
        .map(|(index, size)| {
            let size = size.along(axis);
            if index + 1 == items_count {
                size
            } else {
                size + gap
            }
        })
        .collect();
    state.origins = state
        .sizes
        .iter()
        .scan(px(0.), |cumulative, size| {
            let origin = *cumulative;
            *cumulative += *size;
            Some(origin)
        })
        .collect();
    let total_size = px(state.sizes.iter().map(|size| size.as_f32()).sum::<f32>());
    if axis.is_horizontal() {
        state.content_size.width = total_size;
    } else {
        state.content_size.height = total_size;
    }
}

fn update_reorderable_item_size_layout(
    layout: &mut ItemSizeLayout,
    item_sizes: &ReorderableVirtualListSizes,
    axis: Axis,
    gap: Pixels,
    items_count: usize,
) {
    let identity = Rc::downgrade(&item_sizes.state);
    let same_source = layout
        .reorderable_sizes_identity
        .as_ref()
        .is_some_and(|previous| previous.ptr_eq(&identity));
    let mut source = item_sizes.state.borrow_mut();
    let same_count = layout.sizes.len() == items_count && layout.origins.len() == items_count;
    let same_axis = layout.axis == Some(axis) && layout.gap == gap;
    if same_source && same_count && same_axis && layout.reorderable_sizes_version == source.version
    {
        return;
    }

    let moved_range = if same_source && same_count && same_axis {
        source.dirty_range.take()
    } else {
        None
    };
    if let Some(range) = moved_range {
        let previous_size = layout.sizes[range.clone()].iter().copied().sum::<Pixels>();
        for index in range.clone() {
            let mut item_size = source.values[index].along(axis);
            if index + 1 < items_count {
                item_size += gap;
            }
            layout.sizes[index] = item_size;
        }
        let delta = layout.sizes[range.clone()].iter().copied().sum::<Pixels>() - previous_size;
        let mut origin = layout.origins[range.start];
        for index in range.clone() {
            layout.origins[index] = origin;
            origin += layout.sizes[index];
        }
        if delta != px(0.0) {
            // 尺寸变化时同步移动后续条目；仅重排时总高度不变，无需更新后续坐标。
            for origin in &mut layout.origins[range.end..] {
                *origin += delta;
            }
            if axis.is_horizontal() {
                layout.content_size.width += delta;
            } else {
                layout.content_size.height += delta;
            }
        }
    } else {
        source.dirty_range = None;
        rebuild_item_size_layout(layout, &source.values, axis, gap, items_count);
    }
    // 弱引用保留分配身份，避免旧数据释放后地址复用导致误命中缓存。
    layout.reorderable_sizes_identity = Some(identity);
    layout.reorderable_sizes_version = source.version;
}

impl Styled for VirtualList {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl VirtualList {
    /// 切换虚拟化主轴，复用局部尺寸更新和滚动定位逻辑。
    pub fn with_axis(mut self, axis: Axis) -> Self {
        self.axis = axis;
        self
    }

    pub fn track_scroll(mut self, scroll_handle: &VirtualListScrollHandle) -> Self {
        self.base = self.base.track_scroll(&scroll_handle);
        self.scroll_handle = scroll_handle.clone();
        self
    }

    /// Set the sizing behavior for the list.
    pub fn with_sizing_behavior(mut self, behavior: ListSizingBehavior) -> Self {
        self.sizing_behavior = behavior;
        self
    }

    /// Set the item index used to infer the list's cross-axis size.
    pub fn with_item_to_measure_index(mut self, index: usize) -> Self {
        self.item_to_measure_index = index;
        self
    }

    /// 关闭交叉轴测量时，行填满视口，不再额外构造和布局一行来推断尺寸。
    pub fn with_cross_axis_measurement(mut self, enabled: bool) -> Self {
        self.measure_cross_axis = enabled;
        self
    }

    /// Specify for table.
    ///
    /// Table is special, because the `scroll_handle` is based on Table head (That is not a virtual list).
    #[doc(hidden)]
    pub fn with_scroll_handle(mut self, scroll_handle: &VirtualListScrollHandle) -> Self {
        self.base = div().id(self.id.clone()).size_full();
        self.scroll_handle = scroll_handle.clone();
        self
    }

    fn scroll_to_deferred_item(
        &self,
        scroll_offset: Point<Pixels>,
        item_bounds: Option<Bounds<Pixels>>,
        content_bounds: &Bounds<Pixels>,
        scroll_to_item: DeferredScrollToItem,
    ) -> Point<Pixels> {
        let Some(bounds) = item_bounds else {
            return scroll_offset;
        };

        let mut scroll_offset = scroll_offset;
        match scroll_to_item.strategy {
            ScrollStrategy::Center => {
                if self.axis.is_vertical() {
                    scroll_offset.y = content_bounds.top() + content_bounds.size.height.half()
                        - bounds.top()
                        - bounds.size.height.half()
                } else {
                    scroll_offset.x = content_bounds.left() + content_bounds.size.width.half()
                        - bounds.left()
                        - bounds.size.width.half()
                }
            }
            _ => {
                // Ref: https://github.com/zed-industries/zed/blob/0d145289e0867a8d5d63e5e1397a5ca69c9d49c3/crates/gpui/src/elements/div.rs#L3026
                if self.axis.is_vertical() {
                    if bounds.top() + scroll_offset.y < content_bounds.top() {
                        scroll_offset.y = content_bounds.top() - bounds.top()
                    } else if bounds.bottom() + scroll_offset.y > content_bounds.bottom() {
                        scroll_offset.y = content_bounds.bottom() - bounds.bottom();
                    }
                } else {
                    if bounds.left() + scroll_offset.x < content_bounds.left() {
                        scroll_offset.x = content_bounds.left() - bounds.left();
                    } else if bounds.right() + scroll_offset.x > content_bounds.right() {
                        scroll_offset.x = content_bounds.right() - bounds.right();
                    }
                }
            }
        }
        self.scroll_handle.set_offset(scroll_offset);
        scroll_offset
    }

    /// Ref from: https://github.com/zed-industries/zed/blob/83f9f9d9e3f5914392cab9a09e3472711a1d7b38/crates/gpui/src/elements/uniform_list.rs#L660
    fn measure_item(
        &self,
        list_width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Size<Pixels> {
        if self.items_count == 0 {
            return Size::default();
        }

        if !self.measure_cross_axis {
            let viewport = self
                .scroll_handle
                .state
                .borrow()
                .last_content_size
                .unwrap_or_default();
            return if self.axis.is_vertical() {
                size(viewport.width, px(0.0))
            } else {
                size(px(0.0), viewport.height)
            };
        }

        let item_ix = self.item_to_measure_index.min(self.items_count - 1);
        let mut items = (self.render_items)(item_ix..item_ix + 1, window, cx);
        let Some(mut item_to_measure) = items.pop() else {
            return Size::default();
        };
        let available_space = size(
            list_width.map_or(AvailableSpace::MinContent, |width| {
                AvailableSpace::Definite(width)
            }),
            AvailableSpace::MinContent,
        );
        item_to_measure.layout_as_root(available_space, window, cx)
    }
}

/// Frame state used by the [VirtualItem].
pub struct VirtualListFrameState {
    /// Visible items to be painted.
    items: SmallVec<[AnyElement; 32]>,
    size_layout: Rc<RefCell<ItemSizeLayout>>,
}

#[derive(Default, Clone)]
pub struct ItemSizeLayout {
    axis: Option<Axis>,
    gap: Pixels,
    items_sizes: Rc<Vec<Size<Pixels>>>,
    reorderable_sizes_identity: Option<Weak<RefCell<ReorderableVirtualListSizesState>>>,
    reorderable_sizes_version: u64,
    content_size: Size<Pixels>,
    sizes: Vec<Pixels>,
    origins: Vec<Pixels>,
    last_layout_bounds: Bounds<Pixels>,
}

fn first_item_ending_after(
    item_sizes: &[Pixels],
    item_origins: &[Pixels],
    offset: Pixels,
) -> usize {
    let mut low = 0;
    let mut high = item_sizes.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if item_origins[middle] + item_sizes[middle] <= offset {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

impl IntoElement for VirtualList {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for VirtualList {
    type RequestLayoutState = VirtualListFrameState;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let rem_size = window.rem_size();
        let font_size = window.text_style().font_size.to_pixels(rem_size);
        let mut size_layout = Rc::new(RefCell::new(ItemSizeLayout::default()));
        // Measure vertical lists at the last known content width so relative
        // widths and text truncation resolve like the visible rows; an
        // unconstrained measure would turn one long non-wrapping text into a
        // phantom horizontal scroll range.
        let list_width = if self.axis.is_vertical() {
            self.scroll_handle
                .state
                .borrow()
                .last_content_size
                .map(|size| size.width)
                .filter(|width| !width.is_zero())
        } else {
            None
        };
        let longest_item_size = self.measure_item(list_width, window, cx);

        let layout_id = self.base.interactivity().request_layout(
            global_id,
            inspector_id,
            window,
            cx,
            |style, window, cx| {
                size_layout = window.with_element_state(
                    global_id.unwrap(),
                    |state: Option<Rc<RefCell<ItemSizeLayout>>>, _window| {
                        // 帧状态和滚动句柄共享布局，查询坐标时不复制全量行高数组。
                        let shared = state.unwrap_or_default();
                        let mut state = shared.borrow_mut();

                        // Including the gap between items for calculate the item size
                        let gap = style
                            .gap
                            .along(self.axis)
                            .to_pixels(font_size.into(), rem_size);

                        if let Some(reorderable_sizes) = &self.reorderable_item_sizes {
                            update_reorderable_item_size_layout(
                                &mut state,
                                reorderable_sizes,
                                self.axis,
                                gap,
                                self.items_count,
                            );
                        } else if state.reorderable_sizes_identity.is_some()
                            || state.items_sizes != self.item_sizes
                            || state.axis != Some(self.axis)
                            || state.gap != gap
                        {
                            state.items_sizes = self.item_sizes.clone();
                            state.reorderable_sizes_identity = None;
                            state.reorderable_sizes_version = 0;
                            rebuild_item_size_layout(
                                &mut state,
                                &self.item_sizes,
                                self.axis,
                                gap,
                                self.items_count,
                            );
                        }

                        if self.axis.is_horizontal() {
                            state.content_size.height = longest_item_size.height;
                        } else {
                            state.content_size.width = longest_item_size.width;
                        }

                        drop(state);
                        (shared.clone(), shared)
                    },
                );

                let axis = self.axis;
                let layout_id =
                    match self.sizing_behavior {
                        ListSizingBehavior::Infer => {
                            window.with_text_style(style.text_style().cloned(), |window| {
                                let content_size = size_layout.borrow().content_size;

                                window.request_measured_layout(style, {
                                    move |known_dimensions, available_space, _, _| {
                                        let mut size = Size::default();
                                        if axis.is_horizontal() {
                                            size.width = known_dimensions.width.unwrap_or(
                                                match available_space.width {
                                                    AvailableSpace::Definite(x) => x,
                                                    AvailableSpace::MinContent
                                                    | AvailableSpace::MaxContent => {
                                                        content_size.width
                                                    }
                                                },
                                            );
                                            size.height = known_dimensions.width.unwrap_or(
                                                match available_space.height {
                                                    AvailableSpace::Definite(x) => x,
                                                    AvailableSpace::MinContent
                                                    | AvailableSpace::MaxContent => {
                                                        content_size.height
                                                    }
                                                },
                                            );
                                        } else {
                                            size.width = known_dimensions.width.unwrap_or(
                                                match available_space.width {
                                                    AvailableSpace::Definite(x) => x,
                                                    AvailableSpace::MinContent
                                                    | AvailableSpace::MaxContent => {
                                                        content_size.width
                                                    }
                                                },
                                            );
                                            size.height = known_dimensions.height.unwrap_or(
                                                match available_space.height {
                                                    AvailableSpace::Definite(x) => x,
                                                    AvailableSpace::MinContent
                                                    | AvailableSpace::MaxContent => {
                                                        content_size.height
                                                    }
                                                },
                                            );
                                        }

                                        size
                                    }
                                })
                            })
                        }
                        ListSizingBehavior::Auto => window
                            .with_text_style(style.text_style().cloned(), |window| {
                                window.request_layout(style, None, cx)
                            }),
                    };

                layout_id
            },
        );

        (
            layout_id,
            VirtualListFrameState {
                items: SmallVec::new(),
                size_layout,
            },
        )
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        layout.size_layout.borrow_mut().last_layout_bounds = bounds;
        let style = self
            .base
            .interactivity()
            .compute_style(global_id, None, window, cx);
        let border_widths = style.border_widths.to_pixels(window.rem_size());
        let paddings = style
            .padding
            .to_pixels(bounds.size.into(), window.rem_size());

        let content_bounds = Bounds::from_corners(
            bounds.origin
                + point(
                    border_widths.left + paddings.left,
                    border_widths.top + paddings.top,
                ),
            bounds.bottom_right()
                - point(
                    border_widths.right + paddings.right,
                    border_widths.bottom + paddings.bottom,
                ),
        );

        let axis = self.axis;

        if !self.measure_cross_axis {
            let mut size_layout = layout.size_layout.borrow_mut();
            if axis.is_vertical() {
                size_layout.content_size.width = content_bounds.size.width;
            } else {
                size_layout.content_size.height = content_bounds.size.height;
            }
        }
        let size_layout = layout.size_layout.borrow();
        let item_sizes = &size_layout.sizes;
        let item_origins = &size_layout.origins;

        let mut scroll_state = self.scroll_handle.state.borrow_mut();
        scroll_state.axis = axis;
        scroll_state.items_count = self.items_count;
        scroll_state.last_content_size = Some(content_bounds.size);
        scroll_state.item_layout = Some(layout.size_layout.clone());

        let mut scroll_offset = self.scroll_handle.offset();
        if let Some(scroll_to_item) = scroll_state.deferred_scroll_to_item.take() {
            let index = scroll_to_item.item_index.saturating_add(scroll_to_item.offset);
            let item_bounds = item_origins.get(index).zip(item_sizes.get(index))
                .map(|(&origin, &item_size)| {
                    Bounds {
                        origin: match self.axis {
                            Axis::Horizontal => point(content_bounds.left() + origin, px(0.)),
                            Axis::Vertical => point(px(0.), content_bounds.top() + origin),
                        },
                        size: match self.axis {
                            Axis::Horizontal => size(item_size, content_bounds.size.height),
                            Axis::Vertical => size(content_bounds.size.width, item_size),
                        },
                    }
                });
            scroll_offset = self.scroll_to_deferred_item(
                scroll_offset,
                item_bounds,
                &content_bounds,
                scroll_to_item,
            );
        }
        drop(scroll_state);

        scroll_offset = scroll_offset
            .max(&point(
                content_bounds.size.width - size_layout.content_size.width,
                content_bounds.size.height - size_layout.content_size.height,
            ))
            .min(&point(px(0.), px(0.)));
        if scroll_offset != self.scroll_handle.offset() {
            self.scroll_handle.set_offset(scroll_offset);
        }

        self.base.interactivity().prepaint(
            global_id,
            inspector_id,
            bounds,
            size_layout.content_size,
            window,
            cx,
            |_style, _, hitbox, window, cx| {
                if self.items_count > 0 {
                    let min_scroll_offset = content_bounds.size.along(self.axis)
                        - size_layout.content_size.along(self.axis);

                    let is_scrolled = !scroll_offset.along(self.axis).is_zero();
                    if is_scrolled {
                        match self.axis {
                            Axis::Horizontal if scroll_offset.x < min_scroll_offset => {
                                scroll_offset.x = min_scroll_offset;
                                self.scroll_handle.set_offset(scroll_offset);
                            }
                            Axis::Vertical if scroll_offset.y < min_scroll_offset => {
                                scroll_offset.y = min_scroll_offset;
                                self.scroll_handle.set_offset(scroll_offset);
                            }
                            _ => {}
                        }
                    }

                    let (viewport_start, viewport_end) = match self.axis {
                        Axis::Horizontal => (
                            -(scroll_offset.x + paddings.left),
                            -scroll_offset.x + content_bounds.size.width,
                        ),
                        Axis::Vertical => (
                            -(scroll_offset.y + paddings.top),
                            -scroll_offset.y + content_bounds.size.height,
                        ),
                    };
                    let first_visible_element_ix =
                        first_item_ending_after(item_sizes, item_origins, viewport_start);
                    let last_visible_element_ix = first_item_ending_after(
                        item_sizes,
                        item_origins,
                        viewport_end,
                    )
                    .saturating_add(2)
                    .min(self.items_count);

                    let visible_range = first_visible_element_ix
                        ..cmp::min(last_visible_element_ix, self.items_count);

                    let items = (self.render_items)(visible_range.clone(), window, cx);

                    let content_mask = ContentMask { bounds };
                    window.with_content_mask(Some(content_mask), |window| {
                        for (mut item, ix) in items.into_iter().zip(visible_range.clone()) {
                            let item_origin = match self.axis {
                                Axis::Horizontal => {
                                    content_bounds.origin
                                        + point(item_origins[ix] + scroll_offset.x, scroll_offset.y)
                                }
                                Axis::Vertical => {
                                    content_bounds.origin
                                        + point(scroll_offset.x, item_origins[ix] + scroll_offset.y)
                                }
                            };

                            let available_space = match self.axis {
                                Axis::Horizontal => size(
                                    AvailableSpace::Definite(item_sizes[ix]),
                                    AvailableSpace::Definite(content_bounds.size.height),
                                ),
                                Axis::Vertical => size(
                                    AvailableSpace::Definite(content_bounds.size.width),
                                    AvailableSpace::Definite(item_sizes[ix]),
                                ),
                            };

                            item.layout_as_root(available_space, window, cx);
                            item.prepaint_at(item_origin, window, cx);
                            layout.items.push(item);
                        }
                    });
                }

                hitbox
            },
        )
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.base.interactivity().paint(
            global_id,
            inspector_id,
            bounds,
            hitbox.as_ref(),
            window,
            cx,
            |_, window, cx| {
                for item in &mut layout.items {
                    item.paint(window, cx);
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resized_item_updates_following_offsets_and_total_height() {
        let sizes = ReorderableVirtualListSizes::new(vec![size(px(100.0), px(20.0)); 3]);
        let mut layout = ItemSizeLayout::default();
        update_reorderable_item_size_layout(&mut layout, &sizes, Axis::Vertical, px(0.0), 3);
        sizes.set_size(1, size(px(100.0), px(36.0)));
        update_reorderable_item_size_layout(&mut layout, &sizes, Axis::Vertical, px(0.0), 3);
        assert_eq!(layout.origins, vec![px(0.0), px(20.0), px(56.0)]);
        assert_eq!(layout.content_size.height, px(76.0));
        sizes.set_size(1, size(px(100.0), px(20.0)));
        update_reorderable_item_size_layout(&mut layout, &sizes, Axis::Vertical, px(0.0), 3);
        assert_eq!(layout.origins, vec![px(0.0), px(20.0), px(40.0)]);
        assert_eq!(layout.content_size.height, px(60.0));
    }

    #[test]
    fn changing_axis_rebuilds_reorderable_offsets_before_the_next_move() {
        let sizes = ReorderableVirtualListSizes::new(vec![
            size(px(30.0), px(10.0)),
            size(px(50.0), px(20.0)),
            size(px(70.0), px(40.0)),
        ]);
        let mut layout = ItemSizeLayout::default();
        update_reorderable_item_size_layout(&mut layout, &sizes, Axis::Vertical, px(0.0), 3);
        assert_eq!(layout.origins, vec![px(0.0), px(10.0), px(30.0)]);
        update_reorderable_item_size_layout(&mut layout, &sizes, Axis::Horizontal, px(0.0), 3);
        assert_eq!(layout.origins, vec![px(0.0), px(30.0), px(80.0)]);
        sizes.move_item(0, 2);
        update_reorderable_item_size_layout(&mut layout, &sizes, Axis::Horizontal, px(0.0), 3);
        assert_eq!(layout.origins, vec![px(0.0), px(50.0), px(120.0)]);
        assert_eq!(layout.content_size.width, px(150.0));
    }

    use gpui::{Context, TestAppContext};

    struct VirtualListHarness {
        axis: Axis,
        measure_cross_axis: bool,
        viewport_size: Size<Pixels>,
        item_sizes: Rc<Vec<Size<Pixels>>>,
        scroll_handle: VirtualListScrollHandle,
        visible_ranges: Rc<RefCell<Vec<Range<usize>>>>,
    }

    impl Render for VirtualListHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let item_sizes = self.item_sizes.clone();
            let visible_ranges = self.visible_ranges.clone();
            let scroll_handle = self.scroll_handle.clone();
            virtual_list(
                cx.entity(),
                "virtual-list-test",
                self.axis,
                self.item_sizes.clone(),
                move |_, visible_range, _, _| {
                    let _ = scroll_handle.item_offset(visible_range.start);
                    visible_ranges.borrow_mut().push(visible_range.clone());
                    visible_range
                        .map(|ix| div().w(item_sizes[ix].width).h(item_sizes[ix].height))
                        .collect::<Vec<_>>()
                },
            )
            .track_scroll(&self.scroll_handle)
            .with_cross_axis_measurement(self.measure_cross_axis)
            .w(self.viewport_size.width)
            .h(self.viewport_size.height)
        }
    }

    fn exercise_axis(cx: &mut TestAppContext, axis: Axis, measure_cross_axis: bool) {
        let item_size = match axis {
            Axis::Horizontal => size(px(20.), px(10.)),
            Axis::Vertical => size(px(10.), px(20.)),
        };
        let item_sizes = Rc::new(vec![item_size; 20]);
        let scroll_handle = VirtualListScrollHandle::new();
        let visible_ranges = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let item_sizes = item_sizes.clone();
            let scroll_handle = scroll_handle.clone();
            let visible_ranges = visible_ranges.clone();
            move |_, _| VirtualListHarness {
                axis,
                measure_cross_axis,
                viewport_size: size(px(60.), px(60.)),
                item_sizes,
                scroll_handle,
                visible_ranges,
            }
        });

        cx.update(|window, cx| window.draw(cx).clear(cx));
        let initial_range = visible_ranges.borrow().last().cloned().unwrap();
        assert_eq!(initial_range.start, 0);
        assert!(initial_range.end < item_sizes.len());
        if !measure_cross_axis {
            assert!(visible_ranges.borrow().iter().all(|range| range.len() > 1));
        }
        assert_eq!(scroll_handle.item_offset(0), Some(px(0.)));
        assert_eq!(scroll_handle.item_offset(12), Some(px(240.)));
        assert_eq!(scroll_handle.item_offset(20), Some(px(400.)));
        assert_eq!(scroll_handle.item_offset(21), None);

        scroll_handle.scroll_to_item(12, ScrollStrategy::Top);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let scrolled_range = visible_ranges.borrow().last().cloned().unwrap();
        assert!(scrolled_range.contains(&12));
        match axis {
            Axis::Horizontal => assert!(scroll_handle.offset().x < px(0.)),
            Axis::Vertical => assert!(scroll_handle.offset().y < px(0.)),
        }
        if !measure_cross_axis {
            assert!(visible_ranges.borrow().iter().all(|range| range.len() > 1));
        }
    }

    #[gpui::test]
    fn vertical_visible_range_and_deferred_scroll_are_preserved(cx: &mut TestAppContext) {
        exercise_axis(cx, Axis::Vertical, true);
    }

    #[gpui::test]
    fn horizontal_visible_range_and_deferred_scroll_are_preserved(cx: &mut TestAppContext) {
        exercise_axis(cx, Axis::Horizontal, true);
    }

    #[gpui::test]
    fn viewport_width_skips_measurement_and_preserves_vertical_scrolling(cx: &mut TestAppContext) {
        exercise_axis(cx, Axis::Vertical, false);
    }

    #[gpui::test]
    fn viewport_height_skips_measurement_and_preserves_horizontal_scrolling(cx: &mut TestAppContext) {
        exercise_axis(cx, Axis::Horizontal, false);
    }

    #[gpui::test]
    fn viewport_resize_keeps_single_item_visible_without_horizontal_overflow(cx: &mut TestAppContext) {
        let scroll_handle = VirtualListScrollHandle::new();
        let visible_ranges = Rc::new(RefCell::new(Vec::new()));
        let (view, cx) = cx.add_window_view({
            let scroll_handle = scroll_handle.clone();
            let visible_ranges = visible_ranges.clone();
            move |_, _| VirtualListHarness {
                axis: Axis::Vertical,
                measure_cross_axis: false,
                viewport_size: size(px(60.), px(60.)),
                item_sizes: Rc::new(vec![size(px(10.), px(20.))]),
                scroll_handle,
                visible_ranges,
            }
        });
        for width in [60., 30., 120.] {
            view.update(cx, |view, cx| {
                view.viewport_size.width = px(width);
                cx.notify();
            });
            visible_ranges.borrow_mut().clear();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(*visible_ranges.borrow(), vec![0..1]);
            assert_eq!(scroll_handle.max_offset().x, px(0.));
            assert_eq!(crate::ScrollbarHandle::content_size(&scroll_handle).width, px(width));
            assert_eq!(scroll_handle.item_offset(1), Some(px(20.)));
        }
    }

    #[gpui::test]
    fn deferred_scroll_uses_variable_heights_and_skips_invalid_indices(cx: &mut TestAppContext) {
        let scroll_handle = VirtualListScrollHandle::new();
        let (_, cx) = cx.add_window_view({
            let scroll_handle = scroll_handle.clone();
            move |_, _| VirtualListHarness {
                axis: Axis::Vertical,
                measure_cross_axis: false,
                viewport_size: size(px(60.), px(60.)),
                item_sizes: Rc::new(vec![
                    size(px(10.), px(20.)), size(px(10.), px(80.)), size(px(10.), px(40.)),
                ]),
                scroll_handle,
                visible_ranges: Rc::default(),
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        scroll_handle.scroll_to_item(1, ScrollStrategy::Center);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(scroll_handle.offset().y, px(-30.));
        scroll_handle.scroll_to_item_with_offset(usize::MAX, ScrollStrategy::Top, 2);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(scroll_handle.offset().y, px(-30.));
        scroll_handle.scroll_to_item(2, ScrollStrategy::Top);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(scroll_handle.offset().y, px(-80.));
    }

    #[test]
    fn item_offsets_share_variable_height_layout_and_local_reordering() {
        let values = ReorderableVirtualListSizes::new(vec![
            size(px(10.), px(20.)), size(px(10.), px(80.)), size(px(10.), px(40.)),
        ]);
        let layout = Rc::new(RefCell::new(ItemSizeLayout::default()));
        update_reorderable_item_size_layout(&mut layout.borrow_mut(), &values, Axis::Vertical, px(3.), 3);
        let handle = VirtualListScrollHandle::new();
        handle.state.borrow_mut().item_layout = Some(layout.clone());
        let shared_handle = handle.clone();
        assert_eq!(handle.item_offset(0), Some(px(0.)));
        assert_eq!(handle.item_offset(1), Some(px(23.)));
        assert_eq!(handle.item_offset(2), Some(px(106.)));
        assert_eq!(handle.item_offset(3), Some(px(146.)));
        values.move_item(0, 2);
        update_reorderable_item_size_layout(&mut layout.borrow_mut(), &values, Axis::Vertical, px(3.), 3);
        assert_eq!(shared_handle.item_offset(1), Some(px(83.)));
        assert_eq!(shared_handle.item_offset(2), Some(px(126.)));
        assert_eq!(shared_handle.item_offset(3), Some(px(146.)));
        rebuild_item_size_layout(&mut layout.borrow_mut(), &[size(px(10.), px(50.))], Axis::Vertical, px(0.), 1);
        assert_eq!(handle.item_offset(1), Some(px(50.)));
        assert_eq!(handle.item_offset(2), None);
    }

    #[gpui::test]
    fn empty_list_draws_without_requesting_items(cx: &mut TestAppContext) {
        let visible_ranges = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let visible_ranges = visible_ranges.clone();
            move |_, _| VirtualListHarness {
                axis: Axis::Vertical,
                measure_cross_axis: false,
                viewport_size: size(px(60.), px(60.)),
                item_sizes: Rc::new(Vec::new()),
                scroll_handle: VirtualListScrollHandle::new(),
                visible_ranges,
            }
        });

        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visible_ranges.borrow().is_empty());
    }

    #[test]
    fn replacing_reorderable_sizes_rebuilds_layout_after_source_is_dropped() {
        let mut layout = ItemSizeLayout::default();
        for round in 0..128 {
            for (step, count) in [4, 8, 2, 0, 6, 6].into_iter().enumerate() {
                let height = px(20. + round as f32 + step as f32);
                let values = ReorderableVirtualListSizes::new(vec![size(px(10.), height); count]);
                update_reorderable_item_size_layout(
                    &mut layout,
                    &values,
                    Axis::Vertical,
                    px(3.),
                    count,
                );
                assert_eq!(layout.sizes.len(), count, "替换数据后行高数量必须同步");
                assert_eq!(layout.origins.len(), count, "替换数据后行坐标数量必须同步");
                let mut expected_origin = px(0.);
                for index in 0..count {
                    assert_eq!(layout.origins[index], expected_origin);
                    expected_origin += height;
                    if index + 1 < count {
                        expected_origin += px(3.);
                    }
                }
                assert_eq!(layout.content_size.height, expected_origin);
                drop(values);
            }
        }
    }
}
