// A rendered prompt, with the trusted instruction and the fenced untrusted data told apart.

import { visible } from "../../lib/format";
import { CopyButton, Pill } from "../../ui/primitives";

const FENCE = /^PULSE-UNTRUSTED-DATA-[0-9a-f]+$/;

export function splitPrompt(text: string, delimiter?: string): { instruction: string; preamble: string; data: string | null } {
  const lines = text.split("\n");
  const isFence = (l: string) => (delimiter ? l === delimiter : FENCE.test(l));
  const first = lines.findIndex(isFence);
  if (first < 0) return { instruction: text, preamble: "", data: null };
  const last = lines.findIndex((l, i) => i > first && isFence(l));
  const before = lines.slice(0, first);
  // The paragraph right before the fence introduces the data block.
  let p = before.length - 1;
  while (p >= 0 && before[p] === "") p--;
  const preamble = p >= 0 ? (before[p] ?? "") : "";
  const instruction = before.slice(0, Math.max(0, p)).join("\n").trimEnd();
  const data = lines.slice(first + 1, last < 0 ? undefined : last).join("\n");
  return { instruction, preamble, data };
}

export function PromptView({ text, delimiter, truncated, missing }: { text: string; delimiter?: string; truncated?: boolean; missing?: string[] }) {
  const { instruction, preamble, data } = splitPrompt(text, delimiter);
  return (
    <div className="prompt">
      <div className="prompt-meta">
        {truncated && <Pill tone="attention" title="Something was cut to respect a size limit">truncated</Pill>}
        {missing && missing.length > 0 && (
          <Pill tone="attention" title="These placeholders had no value in the event and rendered as empty">
            empty: {missing.join(", ")}
          </Pill>
        )}
        <span className="spacer" />
        <CopyButton value={text} label="Copy the prompt exactly as sent" />
      </div>
      <div className="prompt-instruction">
        <span className="prompt-label">Instruction (from the rule)</span>
        <pre>{visible(instruction)}</pre>
      </div>
      {data !== null && (
        <div className="prompt-data">
          <span className="prompt-label">Untrusted event data, fenced</span>
          <p className="prompt-preamble">{visible(preamble)}</p>
          <pre>{visible(data)}</pre>
        </div>
      )}
    </div>
  );
}
