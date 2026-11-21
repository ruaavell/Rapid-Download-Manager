import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Progress from "./Progress";
import "./style.css";

const isProgress = new URLSearchParams(window.location.search).get("window") === "progress";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{isProgress ? <Progress /> : <App />}</React.StrictMode>
);