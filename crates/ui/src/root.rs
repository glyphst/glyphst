use anyhow::{Result, anyhow};
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, Render, Window, WindowHandle, WindowId,
};
use gpui_component::Root;
use std::{
    fmt,
    hash::{Hash, Hasher},
};

/// A typed handle to a view hosted below a [`Root`] window.
///
/// GPUI window handles are typed by their root view. Since gpui-component
/// requires that root to be [`Root`], this pairs the real window handle with
/// the typed application view hosted by it.
pub struct RootWindowHandle<V>
where
    V: 'static + Render,
{
    root: WindowHandle<Root>,
    view: Entity<V>,
}

impl<V> RootWindowHandle<V>
where
    V: 'static + Render,
{
    fn new(root: WindowHandle<Root>, view: Entity<V>) -> Self {
        Self { root, view }
    }

    pub fn from_root(root: WindowHandle<Root>, cx: &App) -> Option<Self> {
        let view = root.read(cx).ok()?.view().clone().downcast::<V>().ok()?;
        Some(Self::new(root, view))
    }

    pub fn from_any(window: AnyWindowHandle, cx: &App) -> Option<Self> {
        Self::from_root(window.downcast::<Root>()?, cx)
    }

    pub fn update<C, R>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<V>) -> R,
    ) -> Result<R>
    where
        C: AppContext,
    {
        let view = self.view.clone();
        self.root.update(cx, move |_, window, cx| {
            view.update(cx, |view, cx| update(view, window, cx))
        })
    }

    pub fn entity(&self) -> &Entity<V> {
        &self.view
    }

    pub fn root(&self) -> WindowHandle<Root> {
        self.root
    }

    pub fn window_id(&self) -> WindowId {
        self.root.window_id()
    }

    pub fn is_active(&self, cx: &mut App) -> Option<bool> {
        self.root.is_active(cx)
    }

    pub fn validate(&self, cx: &App) -> Result<()> {
        let root = self.root.read(cx)?;
        let hosted = root
            .view()
            .clone()
            .downcast::<V>()
            .map_err(|_| anyhow!("window does not host the expected inner view"))?;
        if hosted == self.view {
            Ok(())
        } else {
            Err(anyhow!("window inner view handle is stale"))
        }
    }
}

impl<V> Clone for RootWindowHandle<V>
where
    V: 'static + Render,
{
    fn clone(&self) -> Self {
        Self {
            root: self.root,
            view: self.view.clone(),
        }
    }
}

impl<V> fmt::Debug for RootWindowHandle<V>
where
    V: 'static + Render,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootWindowHandle")
            .field("window_id", &self.window_id())
            .field("view", &self.view.entity_id())
            .finish()
    }
}

impl<V> PartialEq for RootWindowHandle<V>
where
    V: 'static + Render,
{
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl<V> Eq for RootWindowHandle<V> where V: 'static + Render {}

impl<V> Hash for RootWindowHandle<V>
where
    V: 'static + Render,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.root.hash(state);
    }
}
