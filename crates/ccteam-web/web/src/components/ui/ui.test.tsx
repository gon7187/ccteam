// v0.8.19 W2 — smoke tests for the primitive layer: variant classes resolve
// and tailwind-merge lets a caller's className override the variant default.

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { Button } from "./button";
import { Badge } from "./badge";
import { Dialog } from "./dialog";
import { Combobox } from "./combobox";
import { SortableHeader } from "./table";
import { MobileTerminalToolbar } from "../MobileTerminalToolbar";
import { ToastDismissButton } from "../Toasts";

describe("ui primitives", () => {
  it("Button applies variant + size classes and defaults type=button", () => {
    const html = renderToString(
      <Button variant="destructive" size="sm">
        x
      </Button>,
    );
    expect(html).toContain("text-status-error");
    expect(html).toContain("h-7");
    expect(html).toContain('type="button"');
  });

  it("tailwind-merge: a passed className wins over the variant default", () => {
    const html = renderToString(<Button className="bg-accent-500">x</Button>);
    expect(html).toContain("bg-accent-500");
    expect(html).not.toContain("bg-brand-500");
  });

  it("Badge renders its variant", () => {
    const html = renderToString(<Badge variant="running">live</Badge>);
    expect(html).toContain("text-status-running");
  });

  it("renders Russian dialog, combobox empty state, and sort labels", () => {
    expect(renderToString(<Dialog lang="ru" open onClose={() => {}} title="x">x</Dialog>)).toContain('aria-label="Закрыть"');
    expect(renderToString(<Combobox lang="ru" value="" onChange={() => {}} options={[]} />)).toContain("Выберите…");
    expect(renderToString(<table><thead><tr><SortableHeader lang="ru" sorted={false}>x</SortableHeader></tr></thead></table>)).toContain('aria-label="Сортировать"');
  });

  it("renders Russian toast dismissal and mobile toolbar accessibility labels", () => {
    expect(renderToString(<ToastDismissButton lang="ru" onDismiss={() => {}} />)).toContain('aria-label="Закрыть"');
    const html = renderToString(<MobileTerminalToolbar lang="ru" sendData={() => {}} termRef={{ current: null }} keyboardHeight={0} ctrlActive={false} onCtrlToggle={() => {}} />);
    expect(html).toContain('aria-label="Стрелка вверх"');
    expect(html).toContain('aria-label="Стрелка вниз"');
  });
});
