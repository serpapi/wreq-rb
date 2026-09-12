# frozen_string_literal: true

require_relative "test_helper"
require "objspace"
require "socket"

class ResponseTest < Minitest::Test
  def test_native_body_is_reported_to_object_space
    with_large_response do |response, body_size, _increase|
      assert_operator ObjectSpace.memsize_of(response), :>=, body_size
    end
  end

  def test_native_body_counts_toward_gc_threshold
    with_large_response do |_response, body_size, increase|
      assert_operator increase, :>=, body_size
    end
  end

  def test_response_methods
    resp = Wreq.get("https://httpbun.com/get")
    assert_kind_of Integer, resp.status
    assert_kind_of String, resp.text
    assert_kind_of String, resp.url
    assert_kind_of Hash, resp.headers
    assert_includes resp.inspect, "Wreq::Response"
  end

  def test_transfer_size_with_compressed_response
    # example.com serves gzip-compressed HTML; transfer_size should be smaller than body
    resp = Wreq.get("https://www.example.com")
    assert_equal 200, resp.status

    body_size = resp.body_bytes.length
    transfer = resp.transfer_size

    assert_kind_of Integer, transfer
    assert transfer > 0, "transfer_size should be positive"
    assert transfer < body_size,
      "transfer_size (#{transfer}) should be less than decompressed body (#{body_size}) for gzip response"
  end

  def test_transfer_size_with_uncompressed_response
    # /robots.txt is small and typically not compressed; sizes should match
    resp = Wreq.get("https://httpbun.com/robots.txt")
    assert_equal 200, resp.status

    body_size = resp.body_bytes.length
    transfer = resp.transfer_size

    assert_kind_of Integer, transfer
    assert_equal body_size, transfer,
      "transfer_size (#{transfer}) should equal body size (#{body_size}) for uncompressed response"
  end

  def test_headers_values_are_arrays
    resp = Wreq.get("https://httpbun.com/get")
    assert_equal 200, resp.status
    headers = resp.headers
    assert_kind_of Hash, headers
    headers.each do |key, value|
      assert_kind_of String, key, "header key should be a String"
      assert_kind_of Array, value, "header value for '#{key}' should be an Array"
      value.each do |v|
        assert_kind_of String, v, "each element in '#{key}' array should be a String"
      end
    end
    assert_equal 1, headers["content-type"].length
  end

  def test_headers_multiple_set_cookie
    client = Wreq::Client.new(redirect: false)
    resp = client.get("https://httpbun.com/cookies/set?a=1&b=2")
    headers = resp.headers
    cookies = headers["set-cookie"]
    assert_kind_of Array, cookies, "set-cookie should be an Array"
    assert cookies.length >= 2,
      "expected at least 2 set-cookie values, got #{cookies.length}: #{cookies.inspect}"
  end

  private

  def with_large_response
    body = "a" * (4 * 1024 * 1024)
    server = TCPServer.new("127.0.0.1", 0)
    server_thread = Thread.new do
      connection = server.accept
      connection.gets("\r\n\r\n")
      connection.write("HTTP/1.1 200 OK\r\nContent-Length: #{body.bytesize}\r\nConnection: close\r\n\r\n")
      connection.write(body)
    ensure
      connection&.close
    end
    client = Wreq::Client.new(emulation: false, http1_only: true, no_proxy: true, timeout: 5)
    was_disabled = GC.disable
    before = GC.stat(:malloc_increase_bytes)
    response = client.get("http://127.0.0.1:#{server.addr[1]}/")
    increase = GC.stat(:malloc_increase_bytes) - before
    server_thread.value
    yield response, body.bytesize, increase
  ensure
    GC.enable unless was_disabled
    server&.close
    server_thread&.kill
    server_thread&.join
  end
end
