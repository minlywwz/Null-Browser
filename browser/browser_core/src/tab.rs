use crate::navigation::{Example, History, Location};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub usize);

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub history: History,
    pub scroll: f32,
}

impl Tab {
    pub fn new(id: TabId, location: Location) -> Self {
        let title = location.label();
        Self {
            id,
            title,
            history: History::new(location),
            scroll: 0.0,
        }
    }

    pub fn location(&self) -> &Location {
        self.history.current()
    }
}

#[derive(Debug)]
pub struct TabManager {
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new(Location::Example(Example::Start))
    }
}

impl TabManager {
    pub fn new(initial: Location) -> Self {
        let mut manager = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
        };
        manager.new_tab(initial);
        manager
    }

    pub fn new_tab(&mut self, location: Location) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab::new(id, location));
        self.active = self.tabs.len() - 1;
        id
    }

    pub fn close_tab(&mut self, id: TabId) -> Option<TabId> {
        let index = self.tabs.iter().position(|t| t.id == id)?;
        self.tabs.remove(index);

        if self.tabs.is_empty() {
            let new_id = TabId(self.next_id);
            self.next_id += 1;
            self.tabs
                .push(Tab::new(new_id, Location::Example(Example::Start)));
            self.active = 0;
        } else if index < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }

        Some(self.active_id())
    }

    pub fn select_tab(&mut self, id: TabId) -> bool {
        if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
            self.active = index;
            true
        } else {
            false
        }
    }

    pub fn select_index(&mut self, index: usize) -> bool {
        if index < self.tabs.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    pub fn select_next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn active_id(&self) -> TabId {
        self.tabs[self.active].id
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn set_active_title(&mut self, title: String) {
        self.tabs[self.active].title = title;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::Visit;

    #[test]
    fn tab_creation_and_defaults() {
        let mut manager = TabManager::default();
        assert_eq!(manager.tab_count(), 1);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), TabId(0));
        assert_eq!(manager.active_tab().title, "Start");
        assert_eq!(
            manager.active_tab().location(),
            &Location::Example(Example::Start)
        );

        let second_id = manager.new_tab(Location::Example(Example::Rendering));
        assert_eq!(second_id, TabId(1));
        assert_eq!(manager.tab_count(), 2);
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.active_id(), TabId(1));
        assert_eq!(manager.active_tab().title, "Rendering");
        assert_eq!(
            manager.active_tab().location(),
            &Location::Example(Example::Rendering)
        );
    }

    #[test]
    fn tab_selection() {
        let mut manager = TabManager::new(Location::Example(Example::Start));
        let tab1 = manager.new_tab(Location::Example(Example::Rendering));
        let tab2 = manager.new_tab(Location::Example(Example::Start));

        assert_eq!(manager.active_id(), tab2);
        assert_eq!(manager.active_index(), 2);

        assert!(manager.select_tab(TabId(0)));
        assert_eq!(manager.active_id(), TabId(0));
        assert_eq!(manager.active_index(), 0);

        assert!(!manager.select_tab(TabId(99)));
        assert_eq!(manager.active_id(), TabId(0));

        assert!(manager.select_index(1));
        assert_eq!(manager.active_id(), tab1);
        assert_eq!(manager.active_index(), 1);

        assert!(!manager.select_index(10));
        assert_eq!(manager.active_id(), tab1);
    }

    #[test]
    fn tab_switching_next_prev_wrap() {
        let mut manager = TabManager::new(Location::Example(Example::Start));
        let tab1 = manager.new_tab(Location::Example(Example::Rendering));
        let tab2 = manager.new_tab(Location::Example(Example::Start));

        manager.select_index(0);
        assert_eq!(manager.active_index(), 0);

        manager.select_next();
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.active_id(), tab1);

        manager.select_next();
        assert_eq!(manager.active_index(), 2);
        assert_eq!(manager.active_id(), tab2);

        // Wrap around next
        manager.select_next();
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), TabId(0));

        // Wrap around prev
        manager.select_prev();
        assert_eq!(manager.active_index(), 2);
        assert_eq!(manager.active_id(), tab2);

        manager.select_prev();
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.active_id(), tab1);

        manager.select_prev();
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), TabId(0));
    }

    #[test]
    fn tab_close_inactive_tabs() {
        let mut manager = TabManager::new(Location::Example(Example::Start));
        let tab1 = manager.new_tab(Location::Example(Example::Rendering));
        let tab2 = manager.new_tab(Location::Example(Example::Start));

        // Select middle tab (tab1 at index 1)
        manager.select_tab(tab1);
        assert_eq!(manager.active_index(), 1);

        // Close tab before active (TabId(0) at index 0)
        let active = manager.close_tab(TabId(0));
        assert_eq!(active, Some(tab1));
        assert_eq!(manager.tab_count(), 2);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), tab1);

        // Close tab after active (tab2 at index 1)
        let active = manager.close_tab(tab2);
        assert_eq!(active, Some(tab1));
        assert_eq!(manager.tab_count(), 1);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), tab1);
    }

    #[test]
    fn tab_close_active_tab() {
        let mut manager = TabManager::new(Location::Example(Example::Start));
        let tab1 = manager.new_tab(Location::Example(Example::Rendering));
        let tab2 = manager.new_tab(Location::Example(Example::Start));

        // Active is tab2 at index 2 (last tab)
        let active = manager.close_tab(tab2);
        assert_eq!(active, Some(tab1));
        assert_eq!(manager.tab_count(), 2);
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.active_id(), tab1);

        // Active is tab1 at index 1 (last tab now)
        let active = manager.close_tab(tab1);
        assert_eq!(active, Some(TabId(0)));
        assert_eq!(manager.tab_count(), 1);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), TabId(0));
    }

    #[test]
    fn tab_close_last_remaining_tab() {
        let mut manager = TabManager::new(Location::Example(Example::Rendering));
        assert_eq!(manager.active_id(), TabId(0));

        let active = manager.close_tab(TabId(0));
        assert_eq!(active, Some(TabId(1)));
        assert_eq!(manager.tab_count(), 1);
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.active_id(), TabId(1));
        assert_eq!(
            manager.active_tab().location(),
            &Location::Example(Example::Start)
        );
        assert_eq!(manager.active_tab().title, "Start");
    }

    #[test]
    fn tab_close_nonexistent() {
        let mut manager = TabManager::default();
        let active = manager.close_tab(TabId(99));
        assert_eq!(active, None);
        assert_eq!(manager.tab_count(), 1);
        assert_eq!(manager.active_id(), TabId(0));
    }

    #[test]
    fn tab_title_and_history_navigation() {
        let mut manager = TabManager::default();
        manager.set_active_title("Custom Page Title".into());
        assert_eq!(manager.active_tab().title, "Custom Page Title");

        let tab = manager.active_tab_mut();
        assert!(!tab.history.can_back());
        assert!(!tab.history.can_forward());

        tab.history
            .commit(Visit::New, Location::Example(Example::Rendering), 42.0);
        assert!(tab.history.can_back());
        assert!(!tab.history.can_forward());
        assert_eq!(tab.location(), &Location::Example(Example::Rendering));

        tab.history
            .commit(Visit::Back, Location::Example(Example::Start), 0.0);
        assert!(!tab.history.can_back());
        assert!(tab.history.can_forward());
        assert_eq!(tab.location(), &Location::Example(Example::Start));

        tab.history
            .commit(Visit::Forward, Location::Example(Example::Rendering), 42.0);
        assert!(tab.history.can_back());
        assert!(!tab.history.can_forward());
        assert_eq!(tab.location(), &Location::Example(Example::Rendering));
    }
}
