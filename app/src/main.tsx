import React from 'react';
import ReactDOM from 'react-dom/client';
import { RouterProvider } from '@tanstack/react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ThemeProvider } from '@sunstead/ui/theme-provider';
import { router } from './router';
import { shouldRetry } from './lib/api';
import './App.css';

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: shouldRetry, staleTime: 1000 } },
});

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ThemeProvider app='atlas'>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </ThemeProvider>
  </React.StrictMode>,
);
