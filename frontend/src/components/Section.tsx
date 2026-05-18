import { JSX, Show } from "solid-js";

interface Props {
  title: string;
  desc?: string;
  children: JSX.Element;
}

/**
 * Section header reads as an instrument label, not an editorial heading:
 * `── output ─────────────────` with the description (if any) pulled to
 * the far right. No counter, no decoration glyph.
 */
export function Section(props: Props) {
  return (
    <section class="section">
      <header class="section-head">
        <span class="section-lead">──</span>
        <span class="section-title">{props.title}</span>
        <span class="section-rule" aria-hidden="true" />
        <Show when={props.desc}>
          <span class="section-desc">{props.desc}</span>
        </Show>
      </header>
      {props.children}
    </section>
  );
}
