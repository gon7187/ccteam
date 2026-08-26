import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { KeyboardFab } from "./KeyboardFab";

describe("KeyboardFab accessible labels", () => {
  it.each([
    ["ru", "Открыть клавиатуру", "Закрыть клавиатуру"],
    ["zh", "打开键盘", "关闭键盘"],
    ["en", "Open keyboard", "Close keyboard"],
  ] as const)("renders %s open and close labels", (lang, open, close) => {
    expect(renderToString(<KeyboardFab lang={lang} keyboardOpen={false} onToggle={() => {}} />)).toContain(`aria-label="${open}"`);
    expect(renderToString(<KeyboardFab lang={lang} keyboardOpen onToggle={() => {}} />)).toContain(`aria-label="${close}"`);
  });
});
