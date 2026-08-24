import { describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";

let terminalState = { connected: false, reconnecting: true, retryCount: 2, retryCountdown: 3, isInScrollback: false };
vi.mock("../hooks/useTerminal", () => ({
  useTerminal: () => ({ containerRef: { current: null }, termRef: { current: null }, state: terminalState, manualReconnect() {}, sendData() {}, exitScrollback() {}, ctrlActiveRef: { current: false }, clearCtrlRef: { current: null } }),
}));
vi.mock("../hooks/useMobileKeyboard", () => ({ useMobileKeyboard: () => ({ isMobile: false, keyboardOpen: false, keyboardHeight: 0, reservedKeyboardHeight: 0 }) }));

import { TerminalView } from "./TerminalView";

describe("TerminalView Russian connection labels", () => {
  it("renders reconnect, loss, and retry in Russian", () => {
    expect(renderToString(<TerminalView lang="ru" slug="demo" />)).toContain("Переподключение через 3 с");
    terminalState = { ...terminalState, reconnecting: false, retryCount: 7 };
    const html = renderToString(<TerminalView lang="ru" slug="demo" />);
    expect(html).toContain("Соединение потеряно");
    expect(html).toContain("Повторить");
  });
});
