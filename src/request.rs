// This file is part of Dirble - https://www.github.com/nccgroup/dirble
// Copyright (C) 2019 Izzy Whistlecroft <Izzy(dot)Whistlecroft(at)nccgroup(dot)com>
// Released as open source by NCC Group Plc - https://www.nccgroup.com/
//
// Dirble is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// Dirble is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with Dirble.  If not, see <https://www.gnu.org/licenses/>.

use crate::{
    arg_parse::{GlobalOpts, HttpVerb},
    content_parse,
};
use curl::{
    Error,
    easy::{Easy2, Handler, WriteError},
};
use log::trace;
use percent_encoding::percent_decode;
use serde::{Serialize, Serializer, ser::SerializeStruct};
use simple_xml_serialize::XMLElement;
use simple_xml_serialize_macro::xml_element;
use std::{sync::Arc, time::Duration};
use url::Url;

pub struct Collector {
    contents: Vec<u8>,
}

impl Collector {
    fn clear_buffer(&mut self) {
        self.contents = Vec::new();
    }

    fn len(&self) -> usize {
        self.contents.len()
    }
}

impl Handler for Collector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        self.contents.extend_from_slice(data);
        Ok(data.len())
    }
}

// Struct which contains information about a response
// This is sent back to the main thread
#[xml_element("path")]
#[derive(Clone, Debug, PartialEq)]
pub struct RequestResponse {
    #[sxs_type_attr]
    pub url: Url,
    #[sxs_type_attr]
    pub code: u32,
    #[sxs_type_attr]
    pub content_len: usize,
    #[sxs_type_attr]
    pub is_directory: bool,
    #[sxs_type_attr]
    pub is_listable: bool,
    #[sxs_type_attr]
    pub redirect_url: String,
    #[sxs_type_attr]
    pub found_from_listable: bool,
    pub parent_index: usize,
    pub parent_depth: u32,
}

impl Serialize for RequestResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_struct("RequestResponse", 8)?;
        s.serialize_field("url", &self.url.as_str())?;
        s.serialize_field("code", &self.code)?;
        s.serialize_field("size", &self.content_len)?;
        s.serialize_field("is_directory", &self.is_directory)?;
        s.serialize_field("is_listable", &self.is_listable)?;
        s.serialize_field("redirect_url", &self.redirect_url)?;
        s.serialize_field("found_from_listable", &self.found_from_listable)?;
        s.end()
    }
}

impl RequestResponse {
    pub fn get_depth(&self) -> i32 {
        let mut depth = self.url.as_str().matches('/').count() as i32;

        if self.url.as_str().ends_with('/') {
            depth -= 1;
        }

        depth -= self.parent_depth as i32;
        depth -= 1;

        depth
    }
}

// This function takes an instance of "Easy2", a base URL and a suffix
// It then makes the request, if the response was not a 404
// then it will return a RequestResponse struct
pub fn make_request(easy: &mut Easy2<Collector>, url: Url) -> RequestResponse {
    trace!("Requesting {}", url);
    // Set the url in the Easy2 instance
    easy.url(url.as_str()).unwrap();

    // Perform the request and check if it's empty
    // If it's empty then return a RequestResponse struct
    match perform(easy) {
        Ok(_v) => {}
        Err(e) => {
            println!("Curl error after requesting {} : {}", url, e);
            let req_response = RequestResponse {
                url,
                code: 0,
                content_len: 0,
                is_directory: false,
                is_listable: false,
                redirect_url: String::from(""),
                found_from_listable: false,
                parent_index: 0,
                parent_depth: 0,
            };
            return req_response;
        }
    }

    // Get the response code
    let code = easy.response_code().unwrap();

    // Declare the RequestResponse for the current request
    let mut req_response = RequestResponse {
        url: url.clone(),
        code,
        content_len: 0,
        is_directory: false,
        is_listable: false,
        redirect_url: String::from(""),
        found_from_listable: false,
        parent_index: 0,
        parent_depth: 0,
    };

    // If the response was a redirect, check if it's a directory
    // Also add the redirect url to the struct
    // Generally, directories will redirect requests to them with no
    // trailing / so that they have a trailing /
    if code == 301 || code == 302 {
        // Obtain and url decode the redirect destination
        let redir_dest = easy.redirect_url().unwrap().unwrap();
        let redir_dest =
            percent_decode(redir_dest.as_bytes()).decode_utf8().unwrap();

        // Clone and url decode the url
        let dir_url = [url.as_str(), "/"].join("");
        let dir_url = percent_decode(dir_url.as_bytes()).decode_utf8().unwrap();

        if dir_url == redir_dest {
            req_response.is_directory = true;
        }

        req_response.redirect_url = redir_dest.to_string();
    }

    // Get the contents of the response and set the length in the struct
    let contents = easy.get_ref();
    req_response.content_len = contents.len();

    req_response
}

pub fn listable_check(
    easy: &mut Easy2<Collector>,
    original_url: Url,
    max_recursion_depth: Option<i32>,
    parent_depth: i32,
    scrape_listable: bool,
) -> Vec<RequestResponse> {
    // Formulate the directory name and make a request to get the
    // contents of the page
    let mut dir_url = String::from(original_url.as_str());
    if !dir_url.ends_with('/') {
        dir_url += "/";
    }
    let mut response =
        make_request(easy, Url::parse(dir_url.as_str()).unwrap());
    let content = get_content(easy).to_lowercase();
    let mut output_list: Vec<RequestResponse> = Vec::new();

    match response.code {
        // If a found response was returned then check if the directory
        // is listable or not
        200 => {
            let listable = content.contains("parent directory")
                || content.contains("up to ")
                || content.contains("directory listing for");

            if listable {
                response.is_listable = true;
                response.is_directory = true;
                output_list.push(response);
            } else {
                response.is_listable = false;
                response.is_directory = true;

                output_list.push(response);
                return output_list;
            }
        }
        // If the code returned was not a 200 then create a struct
        // indicating that this is a folder, then return it
        _ => {
            response.is_directory = true;
            response.is_listable = false;
            output_list.push(response);
            return output_list;
        }
    }

    // If scraping of listables is disabled then just return from the
    // function
    if !scrape_listable {
        return output_list;
    }

    // Get urls scraped from the response
    let scraped_urls: Vec<String> =
        content_parse::scrape_urls(content, dir_url);

    for scraped_url in scraped_urls {
        // If the scraped url doesn't end in a /, it's unlikely to be a
        // folder
        // Add it to the list of found URLs to be returned
        if !scraped_url.ends_with('/') {
            output_list.push(fabricate_request_response(
                Url::parse(scraped_url.as_str()).unwrap(),
                false,
                false,
            ));
        }
        // If the url ends in a /, it is likely to be a folder
        else {
            // If the max recursion depth is set, calculate the URL's depth
            // If the max depth is exceeded then just add the URL to the list
            // Otherwise call this function on the scraped URL
            if let Some(max_depth) = max_recursion_depth {
                let mut depth = scraped_url.matches('/').count() as i32;

                if scraped_url.ends_with('/') {
                    depth -= 1;
                }

                depth -= parent_depth;

                // If we've exceeded the max depth, add the url to the
                // values to be returned
                if depth > max_depth {
                    output_list.push(fabricate_request_response(
                        Url::parse(scraped_url.as_str()).unwrap(),
                        true,
                        false,
                    ));
                } else {
                    output_list.append(&mut listable_check(
                        easy,
                        Url::parse(scraped_url.as_str()).unwrap(),
                        max_recursion_depth,
                        parent_depth,
                        scrape_listable,
                    ));
                }
            }
            // If there is no limit to recursion depth
            // then call this function on the discovered folder
            else {
                output_list.append(&mut listable_check(
                    easy,
                    Url::parse(scraped_url.as_str()).unwrap(),
                    max_recursion_depth,
                    parent_depth,
                    scrape_listable,
                ));
            }
        }
    }

    output_list
}

// Creates an easy2 instance based on the parameters provided by the user
pub fn generate_easy(global_opts: &Arc<GlobalOpts>) -> Easy2<Collector> {
    // Create a new curl Easy2 instance and set it to use GET requests
    let mut easy = Easy2::new(Collector {
        contents: Vec::new(),
    });

    match &global_opts.http_verb {
        HttpVerb::Get => {
            easy.get(true).unwrap();
        }
        HttpVerb::Head => {
            easy.nobody(true).unwrap();
        }
        HttpVerb::Post => {
            easy.post(true).unwrap();
        }
    }

    // Set the timeout of the easy
    easy.timeout(Duration::from_secs(u64::from(global_opts.timeout)))
        .unwrap();

    // Use proxy settings if they have been provided
    if global_opts.proxy_enabled {
        easy.proxy(&global_opts.proxy_address).unwrap();
    }

    // If the ignore cert flag is enabled, ignore cert validity
    if global_opts.ignore_cert {
        easy.ssl_verify_host(false).unwrap();
        easy.ssl_verify_peer(false).unwrap();
    }

    // Set the user agent
    if let Some(user_agent) = &global_opts.user_agent {
        easy.useragent(&user_agent.clone()).unwrap();
    }

    // Set http basic auth options
    if let Some(username) = &global_opts.username {
        easy.username(&username.clone()).unwrap();
        easy.password(&global_opts.password.clone().unwrap())
            .unwrap();
    }

    // Set cookies
    if let Some(cookies) = &global_opts.cookies {
        easy.cookie(cookies).unwrap();
    }

    // Set headers
    if let Some(headers) = &global_opts.headers {
        let mut header_list = curl::easy::List::new();
        for header in headers {
            header_list.append(header).unwrap();
        }
        easy.http_headers(header_list).unwrap();
    }

    easy
}

// Before each request, the buffer should be cleared
// This provides support for chunked http responses
fn perform(easy: &mut Easy2<Collector>) -> Result<(), Error> {
    easy.get_mut().clear_buffer();
    easy.perform()
}

// Get the current content of the given easy and return it as a string
fn get_content(easy: &mut Easy2<Collector>) -> String {
    let contents = easy.get_ref();
    String::from_utf8_lossy(&contents.contents).to_string()
}

// Generate a struct for a response for use when a request hasn't been made
// Used when items were discovered via scraping
pub fn fabricate_request_response(
    url: Url,
    is_directory: bool,
    is_listable: bool,
) -> RequestResponse {
    RequestResponse {
        url,
        code: 0,
        content_len: 0,
        is_directory,
        is_listable,
        redirect_url: String::from(""),
        found_from_listable: true,
        parent_index: 0,
        parent_depth: 0,
    }
}
