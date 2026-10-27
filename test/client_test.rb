# frozen_string_literal: true

require_relative "test_helper"
require "socket"

class ClientTest < Minitest::Test
  def test_client_with_options
    client = Wreq::Client.new(
      user_agent: "wreq-rb-test/0.1",
      timeout: 30
    )
    resp = client.get("https://httpbun.com/headers")
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "wreq-rb-test/0.1", body["headers"]["User-Agent"]
  end

  def test_redirect_client
    client = Wreq::Client.new(redirect: 5)
    resp = client.get("https://httpbun.com/redirect/2")
    assert_equal 200, resp.status
  end

  def test_http1_only
    client = Wreq::Client.new(http1_only: true)
    resp = client.get("https://httpbun.com/get")
    assert_equal 200, resp.status
    assert_equal "HTTP/1.1", resp.version,
      "Expected HTTP/1.1 when http1_only: true, got #{resp.version}"
  end

  def test_http2_only
    client = Wreq::Client.new(http2_only: true)
    resp = client.get("https://httpbun.com/get")
    assert_equal 200, resp.status
    assert_equal "HTTP/2.0", resp.version,
      "Expected HTTP/2.0 when http2_only: true, got #{resp.version}"
  end

  def test_default_headers_with_mixed_types
    client = Wreq::Client.new(
      headers: { :"X-Symbol" => 99, "X-Nil" => nil, "X-Str" => "ok" }
    )
    resp = client.get("https://httpbun.com/headers")
    assert_equal 200, resp.status
    body = resp.json
    assert_equal "99", body["headers"]["X-Symbol"]
    assert_nil body["headers"]["X-Nil"]
    assert_equal "ok", body["headers"]["X-Str"]
  end

  def test_header_order
    order = ["x-zzz", "x-aaa", "x-ccc"]
    client = Wreq::Client.new(
      emulation: false,
      http1_only: true,
      no_proxy: true,
      header_order: order,
      headers: { "x-aaa" => "1", "x-ccc" => "2", "x-zzz" => "3" }
    )
    received = capture_request_headers { |port| client.get("http://127.0.0.1:#{port}/") }.keys

    positions = order.map { |h| received.index(h) }.compact
    assert_equal order.size, positions.size,
      "Not all target headers found in: #{received.inspect}"
    assert_equal positions.sort, positions,
      "Expected #{order.inspect} in order, got positions #{positions.inspect} in: #{received.inspect}"
  end

  def test_header_order_takes_precedence_over_emulation
    # Chrome emulation normally puts "user-agent" before "accept". We reverse
    # that relationship to prove the user's header_order wins.
    order = ["host", "accept", "user-agent"]
    client = Wreq::Client.new(
      emulation: "chrome_145",
      http1_only: true,
      no_proxy: true,
      header_order: order
    )
    received = capture_request_headers { |port| client.get("http://127.0.0.1:#{port}/") }.keys

    positions = order.map { |h| received.index(h) }.compact
    assert_equal order.size, positions.size,
      "Not all target headers found in: #{received.inspect}"
    assert_equal positions.sort, positions,
      "Expected user's header_order #{order.inspect} to take precedence; " \
      "got positions #{positions.inspect} in: #{received.inspect}"
  end

  def test_connect_timeout_defaults_to_client_timeout
    [{ timeout: 0.25 }, { timeout: 0.25, connect_timeout: nil }].each do |options|
      elapsed = stalled_tls_request_duration(options)
      assert_operator elapsed, :<, 2
    end
  end

  def test_explicit_connect_timeout_overrides_client_timeout
    elapsed = stalled_tls_request_duration({ timeout: 0.25, connect_timeout: 0.8 })
    assert_operator elapsed, :>=, 0.6
    assert_operator elapsed, :<, 2
  end

  def test_connect_timeout_without_client_timeout
    elapsed = stalled_tls_request_duration({ connect_timeout: 0.25 })
    assert_operator elapsed, :<, 2
  end

  def test_request_timeout_without_client_timeouts
    elapsed = stalled_tls_request_duration({}, request_timeout: 0.5)
    assert_operator elapsed, :>=, 0.4
  end

  def test_resolve_overrides_dns_for_a_domain
    headers = capture_request_headers do |port|
      client = resolving_client("wreq-resolve.invalid" => "127.0.0.1")
      assert_equal 200, client.get("http://wreq-resolve.invalid:#{port}/").status
    end
    assert_match(/\Awreq-resolve\.invalid:/, headers["host"])
  end

  def test_resolve_honours_the_port_in_the_override
    # The URL carries no explicit port, so the port from the override is used
    # instead of the scheme default (80).
    headers = capture_request_headers do |port|
      client = resolving_client("wreq-resolve.invalid" => "127.0.0.1:#{port}")
      assert_equal 200, client.get("http://wreq-resolve.invalid/").status
    end
    assert_equal "wreq-resolve.invalid", headers["host"]
  end

  def test_resolve_accepts_multiple_addresses
    dead_port = free_port
    headers = capture_request_headers do |port|
      # The first address refuses connections, so wreq falls through to the second.
      client = resolving_client(
        "wreq-resolve.invalid" => ["127.0.0.1:#{dead_port}", "127.0.0.1:#{port}"]
      )
      assert_equal 200, client.get("http://wreq-resolve.invalid/", timeout: 10).status
    end
    assert_equal "wreq-resolve.invalid", headers["host"]
  end

  def test_resolve_ignores_case_and_trailing_dot
    # A differently spelled URL host must not slip past the entry to a real
    # DNS lookup. The Host header keeps the URL's spelling.
    [
      ["wreq-resolve.invalid", "WREQ-Resolve.invalid"],
      ["wreq-resolve.invalid", "wreq-resolve.invalid."],
      ["WREQ-Resolve.invalid.", "wreq-resolve.invalid"],
    ].each do |key, url_host|
      headers = capture_request_headers do |port|
        client = resolving_client(key => "127.0.0.1:#{port}")
        assert_equal 200, client.get("http://#{url_host}/").status
      end
      assert_equal url_host, headers["host"]
    end
  end

  def test_resolve_leaves_other_domains_alone
    # localhost has no entry, so it resolves normally. 192.0.2.1 is reserved
    # for documentation (TEST-NET-1), so applying the entry would fail.
    headers = capture_request_headers do |port|
      client = resolving_client("wreq-resolve.invalid" => "192.0.2.1")
      assert_equal 200, client.get("http://localhost:#{port}/", timeout: 5).status
    end
    assert_match(/\Alocalhost:/, headers["host"])
  end

  def test_resolve_rejects_an_invalid_address
    error = assert_raises(Wreq::Error) do
      Wreq::Client.new(resolve: { "example.com" => "not-an-ip" })
    end
    assert_match(/invalid resolve address for 'example\.com': 'not-an-ip'/, error.message)
  end

  def test_resolve_rejects_hosts_that_can_never_match
    ["example.com:443", "https://example.com", "1.2.3.4", "[::1]", "bücher.example", ""].each do |host|
      error = assert_raises(Wreq::Error, "expected #{host.inspect} to be rejected") do
        Wreq::Client.new(resolve: { host => "127.0.0.1" })
      end
      assert_match(/invalid resolve host/, error.message)
    end
  end

  def test_resolve_rejects_hosts_that_differ_only_in_case
    error = assert_raises(Wreq::Error) do
      Wreq::Client.new(resolve: { "example.com" => "127.0.0.1", "EXAMPLE.com." => "127.0.0.2" })
    end
    assert_match(/duplicate resolve host: 'EXAMPLE\.com\.'/, error.message)
  end

  private

  def resolving_client(overrides)
    Wreq::Client.new(
      emulation: false,
      http1_only: true,
      no_proxy: true,
      resolve: overrides
    )
  end

  def free_port
    server = TCPServer.new("127.0.0.1", 0)
    port = server.addr[1]
    server.close
    port
  end

  # Spins up a one-shot local HTTP/1.1 server, yields its port, and returns the
  # headers of the request it received as a hash with downcased names (in wire
  # order, as Ruby hashes keep insertion order).
  def capture_request_headers
    server = TCPServer.new("127.0.0.1", 0)
    headers = {}
    server_thread = Thread.new do
      conn = server.accept
      conn.gets # skip request line
      while (line = conn.gets&.chomp) && !line.empty?
        name, value = line.split(":", 2)
        headers[name.downcase] = value.strip
      end
      conn.write "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ensure
      conn&.close
    end
    yield server.addr[1]
    # join re-raises anything that went wrong in the server thread.
    assert server_thread.join(5), "Local HTTP server did not finish handling a request"
    headers
  ensure
    server_thread&.kill
    server&.close
  end

  def stalled_tls_request_duration(options, request_timeout: 3)
    server = TCPServer.new("127.0.0.1", 0)
    first_byte = nil
    server_thread = Thread.new do
      connection = server.accept
      first_byte = connection.readpartial(4096).getbyte(0)
      connection.read
    rescue Errno::ECONNRESET
    ensure
      connection&.close
    end

    client = Wreq::Client.new({ emulation: false, http1_only: true, no_proxy: true }.merge(options))
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    assert_raises(Wreq::Error) do
      client.get("https://127.0.0.1:#{server.addr[1]}/", timeout: request_timeout)
    end
    elapsed = Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
    assert server_thread.join(2), "Timed-out TLS connection did not close"
    server_thread.value
    assert_equal 22, first_byte, "Expected a TLS handshake record"
    elapsed
  ensure
    server_thread&.kill
    server_thread&.join
    server&.close
  end
end
