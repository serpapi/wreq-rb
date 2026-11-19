# frozen_string_literal: true

require_relative "test_helper"

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
    received = capture_wire_headers { |url| client.get(url) }

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
    received = capture_wire_headers { |url| client.get(url) }

    positions = order.map { |h| received.index(h) }.compact
    assert_equal order.size, positions.size,
      "Not all target headers found in: #{received.inspect}"
    assert_equal positions.sort, positions,
      "Expected user's header_order #{order.inspect} to take precedence; " \
      "got positions #{positions.inspect} in: #{received.inspect}"
  end

  FIREFOX_HTTP1_HEADERS = %w[
    Host User-Agent Accept Accept-Language Accept-Encoding Connection
    Upgrade-Insecure-Requests Sec-Fetch-Dest Sec-Fetch-Mode Sec-Fetch-Site
    Sec-Fetch-User Priority
  ].freeze

  def test_firefox_http1_header_names_and_order
    (152..158).each do |version|
      client = Wreq::Client.new(emulation: "firefox_#{version}", http1_only: true, no_proxy: true)
      received = capture_wire_headers(spelling: true) { |url| client.get(url) }

      assert_equal FIREFOX_HTTP1_HEADERS, received, "firefox_#{version}"
    end
  end

  def test_firefox_http1_places_request_headers_like_a_navigation
    client = Wreq::Client.new(emulation: "firefox_158", http1_only: true, no_proxy: true)
    received = capture_wire_headers(spelling: true) do |url|
      client.post(url, form: { a: "1" }, headers: {
        "cookie" => "sid=1", "referer" => url, "origin" => url.chomp("/"), "x-custom" => "1"
      })
    end

    assert_equal %w[
      Host User-Agent Accept Accept-Language Accept-Encoding x-custom Content-Type
      Content-Length Origin Connection Referer Cookie Upgrade-Insecure-Requests
      Sec-Fetch-Dest Sec-Fetch-Mode Sec-Fetch-Site Sec-Fetch-User Priority
    ], received
  end

  def test_firefox_http1_places_validators_and_script_headers
    client = Wreq::Client.new(emulation: "firefox_158", http1_only: true, no_proxy: true)
    received = capture_wire_headers(spelling: true) do |url|
      client.get(url, headers: {
        "cache-control" => "no-cache", "pragma" => "no-cache", "if-none-match" => '"v1"',
        "if-modified-since" => "Mon, 02 Nov 2026 10:00:00 GMT", "range" => "bytes=0-1",
        "authorization" => "Bearer t", "sec-gpc" => "1", "dnt" => "1"
      })
    end

    assert_equal %w[
      Host User-Agent Accept Accept-Language Accept-Encoding Authorization Range DNT Sec-GPC
      Connection Upgrade-Insecure-Requests Sec-Fetch-Dest Sec-Fetch-Mode Sec-Fetch-Site
      Sec-Fetch-User If-Modified-Since If-None-Match Priority Pragma Cache-Control
    ], received
  end

  # Headers the profile has no place for keep the order and the spelling the
  # caller gave them, where Firefox writes the headers a script sets.
  FIREFOX_HTTP1_WITH_OWN_HEADERS = %w[
    Host User-Agent Accept Accept-Language Accept-Encoding X-Requested-With X-CSRF-Token x-c
    x-b x-a Connection Upgrade-Insecure-Requests Sec-Fetch-Dest Sec-Fetch-Mode Sec-Fetch-Site
    Sec-Fetch-User Priority
  ].freeze
  OWN_HEADERS = {
    "X-Requested-With" => "XMLHttpRequest", "X-CSRF-Token" => "t", "x-c" => "3", "x-b" => "2", "x-a" => "1"
  }.freeze

  def test_firefox_keeps_the_order_of_request_headers_it_does_not_know
    client = Wreq::Client.new(emulation: "firefox_158", http1_only: true, no_proxy: true)
    received = capture_wire_headers(spelling: true) { |url| client.get(url, headers: OWN_HEADERS) }

    assert_equal FIREFOX_HTTP1_WITH_OWN_HEADERS, received
  end

  def test_firefox_keeps_the_order_of_client_headers_it_does_not_know
    client = Wreq::Client.new(
      emulation: "firefox_158", http1_only: true, no_proxy: true, headers: OWN_HEADERS.first(2).to_h
    )
    received = capture_wire_headers(spelling: true) do |url|
      client.get(url, headers: OWN_HEADERS.drop(2).to_h)
    end

    assert_equal FIREFOX_HTTP1_WITH_OWN_HEADERS, received
  end

  def test_firefox_per_request_emulation_keeps_the_order_of_unknown_headers
    client = Wreq::Client.new(
      emulation: false, http1_only: true, no_proxy: true, headers: OWN_HEADERS.first(2).to_h
    )
    received = capture_wire_headers(spelling: true) do |url|
      client.get(url, emulation: "firefox_158", headers: OWN_HEADERS.drop(2).to_h)
    end

    assert_equal FIREFOX_HTTP1_WITH_OWN_HEADERS, received
  end

  def test_firefox_batch_requests_keep_the_order_of_unknown_headers
    client = Wreq::Client.new(emulation: "firefox_158", http1_only: true, no_proxy: true)
    received = capture_wire_headers(spelling: true) do |url|
      client.request_batch(
        [{ url: url, headers: OWN_HEADERS.drop(2).to_h }], headers: OWN_HEADERS.first(2).to_h
      )
    end

    assert_equal FIREFOX_HTTP1_WITH_OWN_HEADERS, received
  end

  def test_firefox_connection_header_can_be_overridden
    client = Wreq::Client.new(emulation: "firefox_158", http1_only: true, no_proxy: true)
    received = capture_wire_headers(lines: true) do |url|
      client.get(url, headers: { "connection" => "close" })
    end

    assert_equal FIREFOX_HTTP1_HEADERS, received.map { |line| line.split(":", 2).first }
    assert_equal ["Connection: close"], received.grep(/\Aconnection:/i)
  end

  def test_firefox_places_client_headers_without_request_headers
    client = Wreq::Client.new(
      emulation: "firefox_158", http1_only: true, no_proxy: true, headers: OWN_HEADERS
    )
    received = capture_wire_headers(spelling: true) { |url| client.get(url) }

    assert_equal FIREFOX_HTTP1_WITH_OWN_HEADERS, received
  end

  def test_firefox_per_request_emulation_places_client_headers
    # No header on the request itself, or only one the client already sends.
    [{}, { headers: { "x-b" => "9" } }].each do |options|
      [false, "firefox_157"].each do |client_emulation|
        client = Wreq::Client.new(
          emulation: client_emulation, http1_only: true, no_proxy: true, headers: OWN_HEADERS
        )
        received = capture_wire_headers(spelling: true) do |url|
          client.get(url, emulation: "firefox_158", **options)
        end

        assert_equal FIREFOX_HTTP1_WITH_OWN_HEADERS, received, "#{client_emulation.inspect} #{options}"
      end
    end
  end

  def test_firefox_sends_te_unless_http1_is_forced
    # Firefox sends TE over HTTP/2 only. The header is a default one, which
    # cannot depend on the protocol of a connection, so a client that is not
    # restricted to HTTP/1.1 sends it over HTTP/1.1 as well, as it does here.
    client = Wreq::Client.new(emulation: "firefox_158", no_proxy: true)
    received = capture_wire_headers(spelling: true) { |url| client.get(url) }

    assert_equal FIREFOX_HTTP1_HEADERS + ["TE"], received
  end

  def test_firefox_per_request_emulation_uses_firefox_header_names
    # The client has no emulation of its own: its default headers would be
    # sent along with the ones of the per-request profile.
    client = Wreq::Client.new(emulation: false, http1_only: true, no_proxy: true)
    received = capture_wire_headers(spelling: true) { |url| client.get(url, emulation: "firefox_158") }

    assert_equal FIREFOX_HTTP1_HEADERS, received
  end

  def test_header_order_takes_precedence_over_firefox_emulation
    # The user's names keep their position and their spelling; the profile
    # orders and spells only the headers the user did not list.
    order = ["host", "accept", "USER-AGENT"]
    client = Wreq::Client.new(
      emulation: "firefox_158",
      http1_only: true,
      no_proxy: true,
      header_order: order
    )
    received = capture_wire_headers(spelling: true) { |url| client.get(url) }

    assert_equal order + (FIREFOX_HTTP1_HEADERS - %w[Host Accept User-Agent]), received
  end

  def test_header_order_with_firefox_places_unknown_headers
    client = Wreq::Client.new(
      emulation: "firefox_158",
      http1_only: true,
      no_proxy: true,
      header_order: ["host", "x-b"]
    )
    received = capture_wire_headers(spelling: true) do |url|
      client.get(url, headers: { "X-A" => "1", "X-B" => "2", "X-C" => "3" })
    end

    assert_equal %w[host x-b User-Agent Accept Accept-Language Accept-Encoding X-A X-C] +
      FIREFOX_HTTP1_HEADERS.drop(5), received
  end

  def test_header_order_naming_content_type_keeps_unknown_headers_before_the_rest
    client = Wreq::Client.new(
      emulation: "firefox_158",
      http1_only: true,
      no_proxy: true,
      header_order: ["Content-Type"]
    )
    received = capture_wire_headers(spelling: true) do |url|
      client.post(url, json: { a: 1 }, headers: { "X-A" => "1", "X-B" => "2" })
    end

    assert_equal %w[
      Content-Type Host User-Agent Accept Accept-Language Accept-Encoding X-A X-B Content-Length
    ] + FIREFOX_HTTP1_HEADERS.drop(5), received
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

  private

  def stalled_tls_request_duration(options, request_timeout: 3)
    require "socket"
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

  # Spins up a local TCP server, yields the port formatted into a URL, captures
  # the header names from the raw HTTP/1.1 request, then tears down the server.
  # Header names of the request the block sends, in wire order. They are
  # lower-cased unless +spelling+ is true; +lines+ returns the whole header
  # lines instead.
  def capture_wire_headers(spelling: false, lines: false)
    require "socket"
    server = TCPServer.new("127.0.0.1", 0)
    port = server.addr[1]
    received = []
    t = Thread.new do
      conn = server.accept
      conn.gets # skip request line
      loop do
        line = conn.gets&.chomp
        break if line.nil? || line.empty?
        name = line.split(":", 2).first
        received << (lines ? line : (spelling ? name : name.downcase))
      end
      conn.write "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
      conn.close
    rescue
      conn&.close
    end
    yield format("http://127.0.0.1:#{port}/")
    t.join(5)
    server.close
    received
  end
end
