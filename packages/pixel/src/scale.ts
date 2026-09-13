import { screen } from "electron";

import type { EngineInfo } from "./react";
import type { Terminal } from "./terminal";

export function hostDisplayScale(terminal: Terminal | null, env: NodeJS.ProcessEnv): number {
  const explicit = Number(env.PIXEL_DISPLAY_SCALE);
  if (Number.isFinite(explicit) && explicit > 0) return explicit;
  if (terminal?.reportsCssPixels) return 1;
  return screen.getDisplayNearestPoint(screen.getCursorScreenPoint()).scaleFactor;
}

export class CellZoomFollower {
  private last: { height: number; basePx: number } | null = null;

  ratio(info: EngineInfo): number | null {
    const { height, basePx } = info;
    const prev = this.last;
    this.last = { height, basePx };
    if (!prev || !prev.basePx || !prev.height) return null;
    const ratio = basePx / prev.basePx;
    if (!Number.isFinite(ratio) || ratio <= 0 || Math.abs(ratio - 1) < 0.01) return null;
    const paneRatio = height / prev.height;
    if (Math.abs(paneRatio - ratio) < 0.04 * ratio) return null;
    return ratio;
  }
}
