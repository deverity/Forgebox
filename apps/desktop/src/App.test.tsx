import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp } from "./app/testApp";

describe("App", () => {
  it("renders the shell around the first route with the product name", async () => {
    await renderApp();
    expect(await screen.findByRole("main")).toBeInTheDocument();
    // M2-E1-T23: the brand moved to the top of the full-height sidebar.
    expect(screen.getByTestId("sidebar-brand")).toHaveTextContent("Forgebox");
    expect(screen.getByTestId("titlebar")).toHaveTextContent("Dashboard");
    expect(await screen.findByTestId("page-dashboard")).toBeInTheDocument();
  });
});
