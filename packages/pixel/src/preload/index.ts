/// <reference path="../../electron/electron.d.ts" preserve="true" />
/** [placeholder copy: The terminal's colours as rgb triples. `ansi` has 16 slots and is null where the terminal did not report a colour.] */
export interface TerminalTheme {
  background: number[];
  foreground: number[];
  ansi: (number[] | null)[];
}

export interface PixelApi {
  theme(): TerminalTheme | null;
  onTheme(subscriber: (theme: TerminalTheme) => void): () => void;
  quit(): void;
}

declare global {
  // eslint-disable-next-line no-var
  var pixel: PixelApi;
}
