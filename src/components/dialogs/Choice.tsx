import type { ReactNode } from "react";

/** A selectable option row with a radio mark, title and description. */
export function Choice({
  checked,
  onSelect,
  title,
  desc,
  danger,
  disabled,
  children,
}: {
  checked: boolean;
  onSelect: () => void;
  title: ReactNode;
  desc?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
  children?: ReactNode;
}) {
  return (
    <div
      role="radio"
      aria-checked={checked}
      aria-disabled={disabled}
      tabIndex={disabled ? -1 : 0}
      className={`choice ${checked ? "is-checked" : ""} ${danger ? "is-danger" : ""} ${disabled ? "is-disabled" : ""}`}
      onClick={() => !disabled && onSelect()}
      onKeyDown={(e) => {
        if (!disabled && (e.key === " " || e.key === "Enter")) {
          e.preventDefault();
          onSelect();
        }
      }}
    >
      <span className="choice__mark" aria-hidden />
      <span className="choice__text">
        <span className="choice__title">{title}</span>
        {desc && <span className="choice__desc">{desc}</span>}
        {checked && children && <span className="choice__extra" onClick={(e) => e.stopPropagation()}>{children}</span>}
      </span>
    </div>
  );
}
