import { describe, expect, it } from "vitest";
import { parseFrontmatter } from "../frontmatter.js";

describe("skill frontmatter", () => {
  it("reads scalars and lists", () => {
    const parsed = parseFrontmatter('---\nname: notes\ndescription: "Keeps notes tidy."\ntags:\n  - notes\n  - writing\nversion: 2\n---\nSort them.\n');
    expect(parsed.attributes).toEqual({ name: "notes", description: "Keeps notes tidy.", tags: ["notes", "writing"], version: 2 });
    expect(parsed.body).toBe("Sort them.\n");
  });

  it("reads a literal block scalar as its lines", () => {
    const parsed = parseFrontmatter("---\nname: notes\ndescription: |\n  Keeps notes tidy.\n  Use for meeting notes.\ntags:\n  - notes\n---\nbody");
    expect(parsed.attributes.description).toBe("Keeps notes tidy.\nUse for meeting notes.");
    expect(parsed.attributes.tags).toEqual(["notes"]);
  });

  it("folds a folded block scalar into one line, whatever its chomping", () => {
    const parsed = parseFrontmatter("---\ndescription: >-\n  Keeps notes tidy\n  across projects.\n\nname: notes\n---\nbody");
    expect(parsed.attributes.description).toBe("Keeps notes tidy across projects.");
    expect(parsed.attributes.name).toBe("notes");
  });

  it("gives an empty value to a block scalar with nothing under it", () => {
    expect(parseFrontmatter("---\ndescription: |\nname: notes\n---\nbody").attributes).toEqual({ description: "", name: "notes" });
  });
});
