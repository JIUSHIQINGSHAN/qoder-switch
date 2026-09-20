import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/bricolage-grotesque";
import App from "./App";
import "./index.css";
import { installNotificationArchive } from "./lib/notify";
import { applyTheme, getThemePreference, watchSystemTheme } from "./lib/theme";

applyTheme(getThemePreference());
const stopWatchingSystemTheme = watchSystemTheme();
if (import.meta.hot) import.meta.hot.dispose(stopWatchingSystemTheme);

// 提示存档必须在首个 toast 之前装好（包装 sonner 的四类提示）。
installNotificationArchive();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
