import { Component, type ReactNode } from "react";

import { Button, Notice } from "./primitives";

/** Keeps one broken view from blanking the whole window. */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey: string }, { error: Error | null }> {
  override state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  override componentDidUpdate(prev: { resetKey: string }) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) this.setState({ error: null });
  }

  override render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="view-pad">
        <Notice tone="error" title="This view hit a bug" actions={<Button size="sm" onClick={() => this.setState({ error: null })}>Try again</Button>}>
          <p>{this.state.error.message}</p>
          <p className="faint">The rest of the app still works. Pulse itself is not affected: this is only the window.</p>
        </Notice>
      </div>
    );
  }
}
