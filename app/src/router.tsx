import { createRootRoute, createRoute, createRouter, lazyRouteComponent, Outlet } from '@tanstack/react-router';
import { AppLayout } from './layouts/app-layout';
import { useTabsStore } from './lib/stores/tabs';
import { parseSearchParams } from './lib/search-params';

const rootRoute = createRootRoute({
  component: () => (
    <AppLayout>
      <Outlet />
    </AppLayout>
  ),
});

// Defined individually: a helper would erase the router's literal path types.
const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: lazyRouteComponent(() => import('./pages/home-page'), 'HomePage'),
});

const searchRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/search',
  validateSearch: parseSearchParams,
  component: lazyRouteComponent(() => import('./pages/search-page'), 'SearchPage'),
});

const connectionsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings/connections',
  component: lazyRouteComponent(() => import('./pages/connections-page'), 'ConnectionsPage'),
});

const routeTree = rootRoute.addChildren([homeRoute, searchRoute, connectionsRoute]);

export const router = createRouter({ routeTree, defaultPreload: 'intent' });

// The URL is the source of truth for the active tab.
router.subscribe('onResolved', ({ toLocation }) => {
  useTabsStore.getState().syncActive({ pathname: toLocation.pathname, search: toLocation.search });
});

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
