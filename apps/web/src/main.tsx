import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createHashRouter, RouterProvider } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { AppShell } from "@/app/AppShell";
import { Dashboard } from "@/features/Dashboard";
import { Generations } from "@/features/Generations";
import { Traces, TraceDetail } from "@/features/Traces";
import { Sessions, SessionDetail } from "@/features/Sessions";
import { Errors } from "@/features/Errors";
import { Tools } from "@/features/Tools";
import { Harness } from "@/features/Harness";
import { Cost } from "@/features/Cost";
import { Models } from "@/features/Models";
import { Settings } from "@/features/Settings";
import "@/styles/index.css";

// Hash routing: the Rust binary serves one static bundle at /ui and has no
// history fallback, so a path-based deep link would 404 on reload.
const router = createHashRouter([
  {
    path: "/",
    element: <AppShell />,
    children: [
      { index: true, element: <Dashboard /> },
      { path: "traces", element: <Traces /> },
      { path: "traces/:id", element: <TraceDetail /> },
      { path: "generations", element: <Generations /> },
      { path: "sessions", element: <Sessions /> },
      { path: "sessions/:id", element: <SessionDetail /> },
      { path: "errors", element: <Errors /> },
      { path: "tools", element: <Tools /> },
      { path: "harness", element: <Harness /> },
      { path: "cost", element: <Cost /> },
      { path: "models", element: <Models /> },
      { path: "settings", element: <Settings /> },
    ],
  },
]);

const client = new QueryClient({
  defaultOptions: {
    queries: {
      // The SSE stream drives invalidation, so polling would be redundant work.
      refetchOnWindowFocus: false,
      staleTime: 30_000,
      retry: 1,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
