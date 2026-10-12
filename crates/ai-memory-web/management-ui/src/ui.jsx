import React, { useEffect } from "react";

export const primary = "button-primary";
export const secondary = "button-secondary";
export const danger = "button-danger";
export const card = "panel";
// SQLite stores numeric timestamps in microseconds; Date expects milliseconds.
const MICROS_PER_MILLISECOND = 1000;
export const formatTimestamp = (value) =>
  value == null
    ? "—"
    : new Date(
        typeof value === "number" ? value / MICROS_PER_MILLISECOND : value,
      ).toLocaleString();

export function Field({ label, ...props }) {
  return (
    <label className="block text-sm">
      <span className="mb-2 block text-body">{label}</span>
      <input className="w-full" {...props} />
    </label>
  );
}

export function Modal({ title, children, onClose }) {
  const ref = React.useRef(null);
  useEffect(() => {
    ref.current.showModal();
  }, []);
  return (
    <dialog
      ref={ref}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      className="console-dialog m-auto w-[min(95vw,42rem)] rounded-[1.25rem] border border-line bg-surface p-6 text-ink backdrop:bg-ink/25"
    >
      <div className="mb-5 flex items-center justify-between gap-4">
        <h2 className="text-xl font-semibold">{title}</h2>
        <button
          aria-label="Close dialog"
          className={secondary}
          onClick={onClose}
        >
          Close
        </button>
      </div>
      {children}
    </dialog>
  );
}
