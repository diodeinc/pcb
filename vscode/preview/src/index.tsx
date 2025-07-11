import React from "react";
import ReactDOM from "react-dom/client";
import "./index.css";
import App from "./App";
import reportWebVitals from "./reportWebVitals";
import { AvoidLib } from "libavoid-js";

// Initialize libavoid before starting the app
async function initializeApp() {
  try {
    // Load libavoid WebAssembly module
    await AvoidLib.load();
    console.log("Libavoid initialized successfully");
  } catch (error) {
    console.error("Failed to initialize libavoid:", error);
    // Continue anyway - the router will try to initialize on demand
  }

  const root = ReactDOM.createRoot(
    document.getElementById("root") as HTMLElement
  );

  root.render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}

// Start the initialization
initializeApp();

// If you want to start measuring performance in your app, pass a function
// to log results (for example: reportWebVitals(console.log))
// or send to an analytics endpoint. Learn more: https://bit.ly/CRA-vitals
reportWebVitals();
