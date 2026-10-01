import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import { NoticesProvider } from "./notices";
import "./app.css";

const queryClient = new QueryClient({
  defaultOptions: {
    // The server is local and each call redoes real work: fail fast, and refetch after mutations rather than on focus.
    queries: {
      staleTime: 30_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <NoticesProvider>
        <App />
      </NoticesProvider>
    </QueryClientProvider>
  </StrictMode>,
);
