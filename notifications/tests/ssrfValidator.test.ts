import { describe, expect, it, vi } from 'vitest';
import {
  isPrivateIP,
  validateWebhookUrl,
  type DnsResolver,
} from '../src/delivery/ssrfValidator';

const publicIp: DnsResolver = async () => ['93.184.216.34'];
const privateIp: DnsResolver = async () => ['10.0.0.5'];
const mixedIps: DnsResolver = async () => ['93.184.216.34', '192.168.1.10'];
const noAddresses: DnsResolver = async () => [];
const unreachable: DnsResolver = async () => {
  throw new Error('ENOTFOUND');
};

describe('isPrivateIP', () => {
  it.each([
    '10.1.2.3',
    '192.168.0.1',
    '172.16.0.1',
    '172.31.255.255',
    '127.0.0.1',
    '169.254.169.254',
    '0.0.0.0',
    '::1',
    'fc00::1',
    'fd12:3456::1',
    'fe80::1',
    '::ffff:10.0.0.1',
  ])('blocks %s', (ip) => {
    expect(isPrivateIP(ip)).toBe(true);
  });

  it.each(['8.8.8.8', '172.15.0.1', '172.32.0.1', '93.184.216.34', '2001:4860:4860::8888'])(
    'allows %s',
    (ip) => {
      expect(isPrivateIP(ip)).toBe(false);
    },
  );
});

describe('validateWebhookUrl', () => {
  it('blocks localhost before any resolution', async () => {
    const resolver = vi.fn(publicIp);

    await expect(validateWebhookUrl('http://localhost/hook', resolver)).rejects.toThrow(
      'localhost is blocked',
    );
    expect(resolver).not.toHaveBeenCalled();
  });

  it('blocks private IPv4 literals without resolving', async () => {
    const resolver = vi.fn(publicIp);

    await expect(validateWebhookUrl('http://10.0.0.1/hook', resolver)).rejects.toThrow(
      'Private IP 10.0.0.1 is blocked',
    );
    await expect(validateWebhookUrl('http://192.168.1.5/hook', resolver)).rejects.toThrow(
      'Private IP 192.168.1.5 is blocked',
    );
    expect(resolver).not.toHaveBeenCalled();
  });

  it('blocks bracketed private IPv6 literals', async () => {
    await expect(validateWebhookUrl('http://[::1]/hook', publicIp)).rejects.toThrow(
      'Private IP ::1 is blocked',
    );
    await expect(validateWebhookUrl('http://[fe80::1]/hook', publicIp)).rejects.toThrow(
      'Private IP fe80::1 is blocked',
    );
  });

  it('allows public IP literals without resolving', async () => {
    const resolver = vi.fn(publicIp);

    await expect(validateWebhookUrl('http://8.8.8.8/hook', resolver)).resolves.toBe(
      'http://8.8.8.8/hook',
    );
    await expect(validateWebhookUrl('http://[2001:4860:4860::8888]/hook', resolver)).resolves.toBe(
      'http://[2001:4860:4860::8888]/hook',
    );
    expect(resolver).not.toHaveBeenCalled();
  });

  it('allows a hostname that resolves to public addresses', async () => {
    const resolver = vi.fn(publicIp);

    await expect(validateWebhookUrl('https://hooks.example.com/x', resolver)).resolves.toBe(
      'https://hooks.example.com/x',
    );
    expect(resolver).toHaveBeenCalledWith('hooks.example.com');
  });

  it('blocks a hostname that resolves into private ranges', async () => {
    await expect(validateWebhookUrl('https://evil.example.com/x', privateIp)).rejects.toThrow(
      'Host resolves to private IP 10.0.0.5',
    );
    await expect(validateWebhookUrl('https://evil.example.com/x', mixedIps)).rejects.toThrow(
      'Host resolves to private IP 192.168.1.10',
    );
  });

  it('blocks names it cannot resolve', async () => {
    await expect(validateWebhookUrl('https://nope.example.com/x', unreachable)).rejects.toThrow(
      'Could not resolve nope.example.com',
    );
    await expect(validateWebhookUrl('https://nope.example.com/x', noAddresses)).rejects.toThrow(
      'Could not resolve nope.example.com',
    );
  });

  it('uses the system resolver by default', async () => {
    // A public IP literal never reaches DNS, so this stays offline either way.
    await expect(validateWebhookUrl('http://1.1.1.1/hook')).resolves.toBe('http://1.1.1.1/hook');
  });
});
