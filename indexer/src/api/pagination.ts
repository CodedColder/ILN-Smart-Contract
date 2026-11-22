export const DEFAULT_PAGE = 1;
export const DEFAULT_PAGE_SIZE = 20;
export const MAX_PAGE_SIZE = 100;

export interface PageRequest {
  page: number;
  pageSize: number;
  offset: number;
}

function toSafePositiveInt(value: unknown, fallback: number): number {
  const parsed =
    typeof value === 'number' ? value : parseInt(String(value ?? ''), 10);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    return fallback;
  }
  return parsed;
}

/**
 * Normalises `page`/`pageSize` across REST and GraphQL so both surfaces apply
 * the same defaults and page-size ceiling. Invalid or out-of-range values fall
 * back to safe defaults instead of producing `NaN` offsets.
 */
export function parsePagination(
  pageInput: unknown,
  pageSizeInput: unknown,
  options: { defaultPageSize?: number; maxPageSize?: number } = {}
): PageRequest {
  const defaultPageSize = options.defaultPageSize ?? DEFAULT_PAGE_SIZE;
  const maxPageSize = options.maxPageSize ?? MAX_PAGE_SIZE;
  const page = toSafePositiveInt(pageInput, DEFAULT_PAGE);
  const requestedPageSize = toSafePositiveInt(pageSizeInput, defaultPageSize);
  const pageSize = Math.min(requestedPageSize, maxPageSize);
  const rawOffset = (page - 1) * pageSize;
  const offset = Number.isSafeInteger(rawOffset)
    ? rawOffset
    : Number.MAX_SAFE_INTEGER;
  return { page, pageSize, offset };
}
