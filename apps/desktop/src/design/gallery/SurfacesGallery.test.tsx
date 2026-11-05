import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { SurfacesGallery } from "./SurfacesGallery";

describe("SurfacesGallery", () => {
  it("renders every surface and reacts to the toast triggers", async () => {
    const user = userEvent.setup();
    render(<SurfacesGallery />);
    expect(screen.getByRole("heading", { name: "Surfaces and feedback" })).toBeInTheDocument();
    expect(screen.getByRole("list", { name: /ListRow in a VirtualList/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "No sites yet" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Show toast with undo" }));
    expect(await screen.findByRole("button", { name: "Undo" })).toBeInTheDocument();
  });
});
