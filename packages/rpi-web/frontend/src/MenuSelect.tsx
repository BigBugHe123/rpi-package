import { createPortal } from "react-dom";
import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type KeyboardEvent,
} from "react";
import { ChevronDown } from "lucide-react";

export type MenuSelectOption = {
  value: string;
  label: ReactNode;
  disabled?: boolean;
};

type Props = {
  label: string;
  value: string;
  options: MenuSelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  className?: string;
  placeholder?: ReactNode;
  icon?: ReactNode;
};

export function MenuSelect({
  label,
  value,
  options,
  onChange,
  disabled = false,
  className = "",
  placeholder = "—",
  icon,
}: Props) {
  const [open, setOpen] = useState(false);
  const [highlighted, setHighlighted] = useState(-1);
  const anchor = useRef<HTMLDivElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const selectedIndex = options.findIndex((option) => option.value === value);
  const selected = options[selectedIndex];
  const [position, setPosition] = useState({
    top: 0,
    left: 0,
    width: 190,
  });

  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      const target = event.target as Element;
      if (!target.closest("[data-menu-select]") && !target.closest(".dropdown-popover"))
        setOpen(false);
    };
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  function openMenu() {
    if (disabled || !anchor.current) return;
    const box = anchor.current.getBoundingClientRect();
    const width = Math.max(box.width, 190);
    setPosition({
      top: Math.min(box.bottom + 5, window.innerHeight - 220),
      left: Math.max(8, Math.min(box.left, window.innerWidth - width - 8)),
      width,
    });
    setHighlighted(selectedIndex >= 0 ? selectedIndex : 0);
    setOpen(true);
  }

  function choose(option: MenuSelectOption) {
    if (option.disabled) return;
    onChange(option.value);
    setOpen(false);
  }

  function onTriggerKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (open) return;
      openMenu();
    }
  }

  function onMenuKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const enabled = options
      .map((option, index) => (option.disabled ? -1 : index))
      .filter((index) => index >= 0);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!enabled.length) return;
      const current = enabled.indexOf(highlighted);
      const next = event.key === "ArrowDown"
        ? enabled[(current + 1 + enabled.length) % enabled.length]
        : enabled[(current - 1 + enabled.length) % enabled.length];
      setHighlighted(next);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const option = options[highlighted];
      if (option) choose(option);
    }
  }

  useEffect(() => {
    if (!open) return;
    const item = menu.current?.querySelector<HTMLElement>(
      `[data-option-index="${highlighted}"]`,
    );
    item?.scrollIntoView({ block: "nearest" });
  }, [highlighted, open]);

  return (
    <div className={`select-menu ${className}`} data-menu-select ref={anchor}>
      <button
        type="button"
        className="select-menu-trigger"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => (open ? setOpen(false) : openMenu())}
        onKeyDown={onTriggerKeyDown}
      >
        {icon}
        <span className="select-menu-value">
          {selected?.label ?? placeholder}
        </span>
        <ChevronDown className="select-menu-chevron" size={12} aria-hidden="true" />
      </button>
      {open && createPortal(
        <div
          ref={menu}
          className="dropdown-popover select-menu-popover"
          role="listbox"
          aria-label={label}
          tabIndex={-1}
          onKeyDown={onMenuKeyDown}
          style={{
            position: "fixed",
            top: position.top,
            left: position.left,
            width: position.width,
          }}
        >
          {options.map((option, index) => (
            <button
              type="button"
              role="option"
              aria-selected={option.value === value}
              aria-disabled={option.disabled || undefined}
              className={`dropdown-item select-menu-option ${index === highlighted ? "highlighted" : ""}`}
              data-option-index={index}
              key={option.value}
              disabled={option.disabled}
              onMouseEnter={() => setHighlighted(index)}
              onClick={() => choose(option)}
            >
              <span>{option.label}</span>
              {option.value === value && <span aria-hidden="true">✓</span>}
            </button>
          ))}
        </div>,
        document.body,
      )}
    </div>
  );
}
