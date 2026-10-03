/**
 * Open views, modelled as tabs from day one so a tabbed or split layout
 * (flexlayout, as in Solstice) can arrive later without a rewrite.
 *
 * The MVP shows one tab. The URL stays the source of truth for whichever tab
 * is active: the router calls `syncActive` after every navigation, and
 * switching tabs is a navigation to that tab's location.
 */
import { create } from 'zustand';

export interface TabLocation {
  pathname: string;
  search: Record<string, unknown>;
}

export interface Tab {
  id: string;
  location: TabLocation;
}

interface TabsState {
  tabs: Tab[];
  activeId: string;
  /** Records where the active tab now is. */
  syncActive: (location: TabLocation) => void;
  /** Opens a new tab and makes it active. Returns its id. */
  open: (location: TabLocation) => string;
  /** Closes a tab. The last one never closes; it resets to home instead. */
  close: (id: string) => void;
  activate: (id: string) => void;
}

const HOME: TabLocation = { pathname: '/', search: {} };

let next = 1;
const newId = () => `tab-${next++}`;

function initial(): Pick<TabsState, 'tabs' | 'activeId'> {
  const id = newId();
  return { tabs: [{ id, location: HOME }], activeId: id };
}

export const useTabsStore = create<TabsState>()((set, get) => ({
  ...initial(),

  syncActive: (location) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === s.activeId ? { ...t, location } : t)),
    })),

  open: (location) => {
    const id = newId();
    set((s) => ({ tabs: [...s.tabs, { id, location }], activeId: id }));
    return id;
  },

  close: (id) => {
    const { tabs, activeId } = get();
    if (tabs.length === 1) {
      set({ tabs: [{ ...tabs[0], location: HOME }] });
      return;
    }
    const i = tabs.findIndex((t) => t.id === id);
    if (i < 0) return;
    const rest = tabs.filter((t) => t.id !== id);
    // Closing the active tab activates its neighbour, as browsers do.
    const nextActive = id === activeId ? rest[Math.min(i, rest.length - 1)].id : activeId;
    set({ tabs: rest, activeId: nextActive });
  },

  activate: (id) => {
    if (get().tabs.some((t) => t.id === id)) set({ activeId: id });
  },
}));

export function activeTab(state: Pick<TabsState, 'tabs' | 'activeId'>): Tab {
  return state.tabs.find((t) => t.id === state.activeId) ?? state.tabs[0];
}

/** For tests: back to one home tab. */
export function resetTabs() {
  useTabsStore.setState(initial());
}
