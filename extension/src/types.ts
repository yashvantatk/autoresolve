export interface AutoResolveEvent {
  seq: number;
  ts_ms: number;
  run: string;
  role: string;
  kind: string;
  data: Record<string, any>;
}

export interface Edit {
  file: string;
  search: string;
  replace: string;
}

export interface PlanItem {
  title: string;
  summary: string;
  proven: boolean;
  edits: Edit[];
  test?: [string, string];
}

export interface Plan {
  items: PlanItem[];
}

export interface Finding {
  rule: string;
  message: string;
  file: string;
  line: number;
  col: number;
}

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id?: number | string;
  method: string;
  params?: any;
}

export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id?: number | string;
  result?: any;
  error?: {
    code: number;
    message: string;
    data?: any;
  };
}

export interface PipelineProgress {
  runId: string;
  currentRole: string;
  currentKind: string;
  currentTitle?: string;
  percent: number;
  eventsCount: number;
  confirmedIssues?: number;
  provenFixes?: number;
  detail: string;
}
