/**
 * Types for `scripts/seo-plan-source.mjs`.
 *
 * The parser is a plain Node module because the checker and the release tooling
 * run it outside any TypeScript build. The SEO test imports it too — that is the
 * point, one parser — so it needs a shape here.
 */
declare module "*/seo-plan-source.mjs" {
  export const GIB: number;
  export const PLAN_SOURCE: string;
  export interface SeededPlan {
    label: string;
    monthly_price_krw: number;
    active: boolean;
    limits: Record<string, number>;
  }
  /** Every seeded plan, by id. `root` is the repository root. */
  export function seedPlans(root?: string): Promise<Record<string, SeededPlan>>;
  export const ENTERPRISE_SOURCE: string;
  export interface EnterpriseTerms {
    min_channels: number;
    price_per_channel_krw: number;
    storage_per_channel_bytes: number;
    upload_bytes: number;
    broadcasts_per_channel: number;
  }
  /** Enterprise's rate and minimum, from `crates/louver-cloud/src/enterprise.rs`. */
  export function enterpriseTerms(root?: string): Promise<EnterpriseTerms>;
}
