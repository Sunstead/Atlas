import { beforeEach, describe, expect, it } from 'vitest';
import { activeTab, resetTabs, useTabsStore } from './tabs';

const tabs = () => useTabsStore.getState();
const search = (q: string) => ({ pathname: '/search', search: { q } });

describe('tabs store', () => {
  beforeEach(resetTabs);

  it('starts with one home tab', () => {
    expect(tabs().tabs).toHaveLength(1);
    expect(activeTab(tabs()).location.pathname).toBe('/');
  });

  it('follows navigation in the active tab only', () => {
    const first = tabs().activeId;
    tabs().open(search('a'));
    tabs().syncActive(search('b'));
    expect(activeTab(tabs()).location).toEqual(search('b'));
    expect(tabs().tabs.find((t) => t.id === first)!.location.pathname).toBe('/');
  });

  it('activates the neighbour when the active tab closes', () => {
    const a = tabs().activeId;
    const b = tabs().open(search('b'));
    const c = tabs().open(search('c'));
    tabs().activate(b);
    tabs().close(b);
    expect(tabs().activeId).toBe(c);
    tabs().close(c);
    expect(tabs().activeId).toBe(a);
  });

  it('never closes the last tab', () => {
    tabs().syncActive(search('x'));
    tabs().close(tabs().activeId);
    expect(tabs().tabs).toHaveLength(1);
    expect(activeTab(tabs()).location.pathname).toBe('/');
  });

  it('ignores unknown ids', () => {
    const before = tabs().activeId;
    tabs().activate('nope');
    tabs().close('nope');
    expect(tabs().activeId).toBe(before);
  });
});
