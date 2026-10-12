import { useState } from "react";

const themeStorageKey = "ai-memory-management-theme";
function savedTheme() {
  try {
    const value = localStorage.getItem(themeStorageKey);
    return ["light", "dark"].includes(value) ? value : "system";
  } catch {
    return "system";
  }
}
document.documentElement.dataset.theme = savedTheme();

export function ThemePicker() {
  const [theme, setTheme] = useState(savedTheme);
  const chooseTheme = (value) => {
    setTheme(value);
    document.documentElement.dataset.theme = value;
    try {
      if (value === "system") localStorage.removeItem(themeStorageKey);
      else localStorage.setItem(themeStorageKey, value);
    } catch {
      // Theme selection still works when browser storage is unavailable.
    }
  };
  return (
    <div role="group" aria-label="Color theme" className="theme-picker">
      {["system", "light", "dark"].map((value) => (
        <button
          key={value}
          type="button"
          aria-pressed={theme === value}
          onClick={() => chooseTheme(value)}
        >
          {value[0].toUpperCase() + value.slice(1)}
        </button>
      ))}
    </div>
  );
}
