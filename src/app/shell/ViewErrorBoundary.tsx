import { Component, type ErrorInfo, type ReactNode } from "react";

import { Button } from "../../ui/components/Button";
import { ErrorState } from "../../ui/components/States";

interface Props {
  /** What failed, e.g. "Files view" or "main.typ editor". */
  name: string;
  children: ReactNode;
  /** One-line treatment for tight places (status bar). */
  compact?: boolean;
}

interface State {
  error: Error | null;
}

/** Contains a crashing contributed view/editor so the rest of the workbench keeps working. */
export class ViewErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(`${this.props.name} crashed`, error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    if (this.props.compact) {
      return (
        <span className="wb-inline-error" title={error.message}>
          {this.props.name} failed
        </span>
      );
    }
    return (
      <ErrorState
        title={`${this.props.name} stopped working`}
        description="The rest of the workbench is unaffected. Reload the view to try again."
        details={error.message}
        actions={
          <Button variant="secondary" onPress={() => this.setState({ error: null })}>
            Reload view
          </Button>
        }
      />
    );
  }
}
