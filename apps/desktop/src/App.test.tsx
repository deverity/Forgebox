import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp } from "./app/testApp";

describe("App", () => {
  it("renders the shell around the first route with the product name", async () => {
    await renderApp();
    expect(await screen.findByRole("main")).toBeInTheDocument();
    expect(screen.getByTestId("titlebar")).toHaveTextContent("Forgebox");
    expect(await screen.findByTestId("page-dashboard")).toBeInTheDocument();
  });
});
