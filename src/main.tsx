import React from "react";
import ReactDOM from "react-dom/client";
import App from './App';
import './i18n'; // Import i18n config
import "./App.css";

import { isTauri } from "./utils/env";
// Explicitly call the Rust command to show the window on startup
// Used together with visible:false to fix the startup black-screen issue
if (isTauri()) {
  import("@tauri-apps/api/core").then(({ invoke }) => {
    invoke("show_main_window").catch(console.error);
  });
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />

  </React.StrictMode>,
);
