import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "./App";

describe("App", () => {
  it("identifies the Windows application", () => {
    render(<App />);

    expect(
      screen.getByRole("heading", { level: 1, name: "NetsuStack" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Windows supervisor workspace")).toBeVisible();
  });
});
