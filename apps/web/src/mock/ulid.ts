// Monotonic ULIDs, like the daemon's `ulid::Generator`: ids made in one process sort in the
// order they were made, even within one millisecond.

const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

function encodeTime(ms: number): string {
  let out = "";
  let t = ms;
  for (let i = 0; i < 10; i++) {
    out = (ALPHABET[t % 32] ?? "0") + out;
    t = Math.floor(t / 32);
  }
  return out;
}

export class UlidGenerator {
  private lastMs = -1;
  private random: number[] = [];

  constructor(private readonly rand: () => number = Math.random) {}

  next(ms: number): string {
    if (ms <= this.lastMs) {
      // Same (or earlier) millisecond: increment the random part.
      ms = this.lastMs;
      for (let i = this.random.length - 1; i >= 0; i--) {
        const v = (this.random[i] ?? 0) + 1;
        if (v < 32) {
          this.random[i] = v;
          break;
        }
        this.random[i] = 0;
      }
    } else {
      this.lastMs = ms;
      this.random = Array.from({ length: 16 }, () => Math.floor(this.rand() * 32));
    }
    return encodeTime(ms) + this.random.map((v) => ALPHABET[v]).join("");
  }
}
