import React, { Component, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { AlertCircle, RefreshCw } from "lucide-react";

interface Props {
  children: ReactNode;
  fallback?: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  public state: State = {
    hasError: false,
    error: null,
  };

  public static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  public componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error("未捕获的渲染异常:", error, errorInfo);
  }

  private handleReload = () => {
    window.location.reload();
  };

  private handleReset = () => {
    this.setState({ hasError: false, error: null });
  };

  public render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback;
      }
      return (
        <div className="flex h-full min-h-[300px] flex-col items-center justify-center p-6 text-center">
          <div className="flex size-12 items-center justify-center rounded-full bg-destructive/10 text-destructive mb-4">
            <AlertCircle className="size-6" />
          </div>
          <h2 className="text-base font-semibold text-foreground mb-1">页面组件加载异常</h2>
          <p className="max-w-md text-xs text-muted-foreground mb-4">
            {this.state.error?.message || "发生未知前端渲染错误，已阻止整页白屏。"}
          </p>
          <div className="flex items-center gap-3">
            <Button size="sm" variant="outline" onClick={this.handleReset}>
              尝试重试
            </Button>
            <Button size="sm" onClick={this.handleReload} className="gap-1.5">
              <RefreshCw className="size-3.5" />
              刷新页面
            </Button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
