import React from "react";
import ReactDOM from "react-dom/client";
import "./index.css";
import App from "./App";
import { TooltipProvider } from "@/components/ui/tooltip";

// Palenight is a dark scheme: shadcn's tokens switch on the `dark` class.
document.documentElement.classList.add("dark");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <TooltipProvider delay={80}>
      <App />
    </TooltipProvider>
  </React.StrictMode>,
);
