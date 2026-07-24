import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { ClientProvider } from "./lib/ClientProvider";
import { TauriClient } from "./lib/client";
import "./index.css";

const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 5_000 } } });

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={qc}>
      <ClientProvider client={new TauriClient()}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </ClientProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
