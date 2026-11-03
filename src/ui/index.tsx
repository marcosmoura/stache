import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import { WindowRouter } from './src/shell/components/WindowRouter';

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      staleTime: Infinity,
    },
  },
});

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <WindowRouter />
    </QueryClientProvider>
  );
}
