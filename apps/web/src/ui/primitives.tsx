// The app's controls. Untrusted strings only ever reach the DOM as text, through `visible()`.

import {
  createContext,
  forwardRef,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from "react";

import { useTildify } from "../app/home";
import { formatKeys } from "../app/keys";
import type { Tone } from "../lib/explain";
import { CLOCK_TICK_MS, relative, splitForMiddle, visible } from "../lib/format";
import { Icon, type IconName } from "./icons";

export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

// ---- buttons ----

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "default" | "primary" | "danger" | "ghost" | "attention";
  size?: "sm" | "md";
  icon?: IconName;
  /** Shown in the tooltip and, for `md` buttons, on the button. */
  shortcut?: string;
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "default", size = "md", icon, shortcut, className, children, title, type, ...rest },
  ref,
) {
  const tip = shortcut ? `${title ?? (typeof children === "string" ? children : "")} (${formatKeys(shortcut).join(" then ")})`.trim() : title;
  return (
    <button ref={ref} type={type ?? "button"} className={cx("btn", `btn-${variant}`, `btn-${size}`, className)} title={tip} {...rest}>
      {icon && <Icon name={icon} size={14} />}
      {children !== undefined && children !== null && <span>{children}</span>}
      {shortcut && size === "md" && <Kbd keys={shortcut} subtle />}
    </button>
  );
});

export function IconButton({ icon, label, ...rest }: Omit<ButtonProps, "children" | "icon"> & { icon: IconName; label: string }) {
  return <Button {...rest} icon={icon} aria-label={label} title={rest.title ?? label} className={cx("btn-icon", rest.className)} />;
}

export function Kbd({ keys, subtle }: { keys: string; subtle?: boolean }) {
  return (
    <span className={cx("kbd-group", subtle && "kbd-subtle")} aria-hidden="true">
      {formatKeys(keys).map((k, i) => (
        <kbd key={i}>{k}</kbd>
      ))}
    </span>
  );
}

// ---- status ----

export function Pill({ tone = "neutral", children, title, icon }: { tone?: Tone; children: ReactNode; title?: string; icon?: IconName }) {
  return (
    <span className={cx("pill", `tone-${tone}`)} title={title}>
      {icon && <Icon name={icon} size={12} />}
      {children}
    </span>
  );
}

export function Dot({ tone = "neutral", label }: { tone?: Tone; label?: string }) {
  return <span className={cx("dot", `tone-${tone}`)} role={label ? "img" : undefined} aria-label={label} title={label} />;
}

// ---- text ----

/** Machine data from outside: rendered as text with hidden characters made visible. */
export function Mono({ children, className, title }: { children: string; className?: string; title?: string }) {
  return (
    <span className={cx("mono", className)} title={title}>
      {visible(children, { singleLine: true })}
    </span>
  );
}

/** One line that truncates in the middle, keeping the file name; the full value is copyable. */
export function Trunc({ text, mono = true, className, copy = false, tail }: { text: string; mono?: boolean; className?: string; copy?: boolean; tail?: number }) {
  // Paths under the home directory read as `~/…`; the title and the copy button keep it whole.
  const shown = visible(useTildify()(text), { singleLine: true });
  const [head, end] = splitForMiddle(shown, tail);
  return (
    <span className={cx("trunc", mono && "mono", className)} title={visible(text, { singleLine: true })}>
      <span className="trunc-head">{head}</span>
      {end && <span className="trunc-tail">{end}</span>}
      {copy && <CopyButton value={text} />}
    </span>
  );
}

export function CopyButton({ value, label = "Copy" }: { value: string; label?: string }) {
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setDone(false), 1200);
    return () => clearTimeout(t);
  }, [done]);
  return (
    <button
      type="button"
      className="copy"
      aria-label={done ? "Copied" : label}
      title={done ? "Copied" : label}
      onClick={(e) => {
        e.stopPropagation();
        void navigator.clipboard?.writeText(value).then(() => setDone(true), () => setDone(false));
      }}
    >
      <Icon name={done ? "check" : "copy"} size={12} />
    </button>
  );
}

// A shared, coarse clock for relative times. It ticks only while something shows one.
const clockListeners = new Set<() => void>();
let clockNow = Date.now();
let clockTimer: ReturnType<typeof setInterval> | null = null;
function clockSubscribe(l: () => void): () => void {
  clockListeners.add(l);
  if (!clockTimer) {
    clockTimer = setInterval(() => {
      if (document.visibilityState !== "visible") return;
      clockNow = Date.now();
      for (const x of clockListeners) x();
    }, CLOCK_TICK_MS);
  }
  return () => {
    clockListeners.delete(l);
    if (clockListeners.size === 0 && clockTimer) {
      clearInterval(clockTimer);
      clockTimer = null;
    }
  };
}

export function useNow(): number {
  return useSyncExternalStore(clockSubscribe, () => clockNow);
}

/** A seconds-precise clock for things that are running right now. */
export function useTicker(active: boolean, everyMs = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(t);
  }, [active, everyMs]);
  return now;
}

export function Ago({ iso, className }: { iso: string; className?: string }) {
  const now = useNow();
  return (
    <time className={cx("num", className)} dateTime={iso} title={new Date(iso).toLocaleString()}>
      {relative(iso, Math.max(now, Date.now() - 10_000))}
    </time>
  );
}

// ---- layout ----

export function Section({ title, actions, children, className, id }: { title: ReactNode; actions?: ReactNode; children: ReactNode; className?: string; id?: string }) {
  return (
    <section className={cx("section", className)} aria-labelledby={id}>
      <header className="section-head">
        <h3 id={id}>{title}</h3>
        {actions && <div className="section-actions">{actions}</div>}
      </header>
      {children}
    </section>
  );
}

export function Empty({ title, children, action, tone }: { title: string; children?: ReactNode; action?: ReactNode; tone?: Tone }) {
  return (
    <div className={cx("empty", tone && `tone-${tone}`)}>
      <p className="empty-title">{title}</p>
      {children && <div className="empty-body">{children}</div>}
      {action && <div className="empty-action">{action}</div>}
    </div>
  );
}

export function Dl({ items }: { items: [ReactNode, ReactNode][] }) {
  return (
    <dl className="dl">
      {items.map(([k, v], i) => (
        <div key={i} className="dl-row">
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Notice({ tone, title, children, actions }: { tone: Tone; title: ReactNode; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className={cx("notice", `tone-${tone}`)} role={tone === "error" ? "alert" : undefined}>
      <div className="notice-text">
        <strong>{title}</strong>
        {children && <div className="notice-body">{children}</div>}
      </div>
      {actions && <div className="notice-actions">{actions}</div>}
    </div>
  );
}

// ---- fields ----

/** The id a `Field`'s control takes when it has none, so the label names it. */
const FieldControlId = createContext<string | undefined>(undefined);

export function Field({
  label,
  hint,
  error,
  children,
  htmlFor,
  wide,
}: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null | undefined;
  children: ReactNode;
  htmlFor?: string;
  wide?: boolean;
}) {
  const auto = useId();
  const id = htmlFor ?? auto;
  return (
    <div className={cx("field", wide && "field-wide", error && "field-invalid")}>
      <label className="field-label" htmlFor={id}>
        {label}
      </label>
      <div className="field-control">
        <FieldControlId.Provider value={htmlFor ? undefined : id}>{children}</FieldControlId.Provider>
      </div>
      {error ? (
        <p className="field-error" role="alert">
          {error}
        </p>
      ) : hint ? (
        <p className="field-hint">{hint}</p>
      ) : null}
    </div>
  );
}

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement> & { mono?: boolean }>(function Input(
  { className, mono, ...rest },
  ref,
) {
  const fieldId = useContext(FieldControlId);
  return <input ref={ref} className={cx("input", mono && "mono", className)} spellCheck={false} autoComplete="off" {...rest} id={rest.id ?? fieldId} />;
});

export const TextArea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement> & { mono?: boolean }>(function TextArea(
  { className, mono, ...rest },
  ref,
) {
  const fieldId = useContext(FieldControlId);
  return <textarea ref={ref} className={cx("input", "textarea", mono && "mono", className)} spellCheck={false} {...rest} id={rest.id ?? fieldId} />;
});

export function Select({ className, children, ...rest }: SelectHTMLAttributes<HTMLSelectElement>) {
  const fieldId = useContext(FieldControlId);
  return (
    <select className={cx("input", "select", className)} {...rest} id={rest.id ?? fieldId}>
      {children}
    </select>
  );
}

export function Toggle({ checked, onChange, label, hint, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: ReactNode; hint?: ReactNode; disabled?: boolean }) {
  const id = useId();
  return (
    <div className="toggle">
      <input id={id} type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <label htmlFor={id}>
        <span>{label}</span>
        {hint && <span className="toggle-hint">{hint}</span>}
      </label>
    </div>
  );
}

/** Editable list of short strings (type globs, scopes, args). */
export function ListInput({
  values,
  onChange,
  placeholder,
  suggestions,
  problem,
  mono = true,
  id,
}: {
  values: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
  suggestions?: string[];
  problem?: (v: string) => string | null;
  mono?: boolean;
  id?: string;
}) {
  const [draft, setDraft] = useState("");
  const listId = useId();
  const add = () => {
    const v = draft.trim();
    if (!v) return;
    if (!values.includes(v)) onChange([...values, v]);
    setDraft("");
  };
  return (
    <div className="listinput">
      {values.length > 0 && (
        <ul className="chips">
          {values.map((v, i) => {
            const p = problem?.(v) ?? null;
            return (
              <li key={`${v}-${i}`} className={cx("chip", p && "chip-invalid")} title={p ?? v}>
                <span className={cx(mono && "mono")}>{visible(v, { singleLine: true })}</span>
                <button type="button" aria-label={`Remove ${v}`} onClick={() => onChange(values.filter((_, j) => j !== i))}>
                  <Icon name="x" size={10} />
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <div className="listinput-add">
        <Input
          id={id}
          mono={mono}
          value={draft}
          placeholder={placeholder}
          list={suggestions ? listId : undefined}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === ",") {
              e.preventDefault();
              add();
            } else if (e.key === "Backspace" && !draft && values.length) {
              onChange(values.slice(0, -1));
            }
          }}
          onBlur={add}
        />
        {suggestions && (
          <datalist id={listId}>
            {suggestions.map((s) => (
              <option key={s} value={s} />
            ))}
          </datalist>
        )}
      </div>
    </div>
  );
}

// ---- dialogs ----

/** A modal on the native <dialog>: focus is trapped and Escape closes it. */
export function Modal({ open, onClose, title, children, actions, wide }: { open: boolean; onClose: () => void; title: ReactNode; children: ReactNode; actions?: ReactNode; wide?: boolean }) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className={cx("modal", wide && "modal-wide")}
      aria-labelledby={titleId}
      onClose={onClose}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onMouseDown={(e) => {
        if (e.target === ref.current) onClose();
      }}
    >
      {open && (
        <div className="modal-inner">
          <h2 className="modal-title" id={titleId}>
            {title}
          </h2>
          <div className="modal-body">{children}</div>
          {actions && <div className="modal-actions">{actions}</div>}
        </div>
      )}
    </dialog>
  );
}

export function Confirm({
  open,
  title,
  children,
  confirm,
  danger,
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: string;
  children: ReactNode;
  confirm: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={title}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant={danger ? "danger" : "primary"}
            autoFocus
            onClick={() => {
              onConfirm();
              onClose();
            }}
          >
            {confirm}
          </Button>
        </>
      }
    >
      {children}
    </Modal>
  );
}

/** A small menu anchored to its button, on the native popover API. */
export function Menu({ label, icon, items, size = "sm", variant = "default" }: { label: string; icon?: IconName; size?: "sm" | "md"; variant?: ButtonProps["variant"]; items: { label: string; onSelect: () => void; hint?: string }[] }) {
  const id = useId().replace(/:/g, "");
  const ref = useRef<HTMLDivElement>(null);
  const btn = useRef<HTMLButtonElement>(null);
  return (
    <>
      <Button
        ref={btn}
        size={size}
        variant={variant}
        {...(icon ? { icon } : {})}
        aria-haspopup="menu"
        popoverTarget={`menu-${id}`}
        onClick={() => {
          const r = btn.current?.getBoundingClientRect();
          const m = ref.current;
          if (r && m) {
            m.style.top = `${r.bottom + 4}px`;
            m.style.left = `${Math.min(r.left, window.innerWidth - 220)}px`;
          }
        }}
      >
        {label}
      </Button>
      <div ref={ref} id={`menu-${id}`} popover="auto" role="menu" className="menu">
        {items.map((it) => (
          <button
            key={it.label}
            type="button"
            role="menuitem"
            className="menu-item"
            onClick={() => {
              ref.current?.hidePopover();
              it.onSelect();
            }}
          >
            <span>{it.label}</span>
            {it.hint && <span className="menu-hint">{it.hint}</span>}
          </button>
        ))}
      </div>
    </>
  );
}
